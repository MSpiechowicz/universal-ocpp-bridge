use std::{
    fs,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use uuid::Uuid;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("uob-cli-test-{}", Uuid::new_v4()));
        fs::create_dir(&path).expect("create test directory");
        Self(path)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn uob() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_uob"));
    command.stdin(Stdio::null());
    command
}

fn write_configuration(path: &Path, address: SocketAddr, extra: &str) {
    fs::write(
        path,
        format!(
            "[bridge]\nid = 'test-bridge'\nenvironment = 'demo'\n\n[management]\nlisten_addr = '{address}'\n{extra}"
        ),
    )
    .expect("write configuration");
}

#[test]
fn config_check_has_machine_output_and_sanitized_failures() {
    let directory = TestDirectory::new();
    let valid = directory.path("valid.toml");
    write_configuration(&valid, "127.0.0.1:0".parse().unwrap(), "");
    let result = uob()
        .args(["config", "check", "--config"])
        .arg(&valid)
        .output()
        .expect("run config check");
    assert!(result.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&result.stdout).unwrap(),
        serde_json::json!({ "status": "valid" })
    );
    assert!(result.stderr.is_empty());

    let invalid = directory.path("invalid.toml");
    let secret = "do-not-print-this-credential";
    fs::write(
        &invalid,
        format!("[bridge]\nid='test'\nunknown='{secret}'\n"),
    )
    .unwrap();
    let result = uob()
        .args(["config", "check", "--config"])
        .arg(&invalid)
        .output()
        .expect("run invalid config check");
    assert_eq!(result.status.code(), Some(2));
    assert!(result.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&result.stderr).contains(secret));

    let result = uob()
        .args(["serve", "--config"])
        .arg(&invalid)
        .output()
        .expect("run invalid service configuration");
    assert_eq!(result.status.code(), Some(2));
    assert!(result.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&result.stderr).contains(secret));
}

#[test]
fn serve_with_closed_stdin_keeps_api_but_disables_assets() {
    let directory = TestDirectory::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let configuration = directory.path("bridge.toml");
    write_configuration(&configuration, address, "");
    let browser_marker = directory.path("browser-launched");
    let browser = directory.path("browser");
    fs::write(&browser, "#!/bin/sh\ntouch \"$UOB_BROWSER_MARKER\"\n").unwrap();
    #[cfg(unix)]
    fs::set_permissions(&browser, fs::Permissions::from_mode(0o755)).unwrap();

    let mut child = uob()
        .args(["serve", "--config"])
        .arg(&configuration)
        .arg("--no-ui")
        .env("BROWSER", &browser)
        .env("UOB_BROWSER_MARKER", &browser_marker)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start service");
    wait_until_listening(address, &mut child);

    let root = http_get(address, "/");
    let identity = http_get(address, "/api/v1/identity");
    assert!(root.starts_with("HTTP/1.1 404"));
    assert!(identity.starts_with("HTTP/1.1 200"));
    assert!(!browser_marker.exists(), "service launched a browser");

    child.kill().expect("stop test service");
    let output = child.wait_with_output().expect("collect service output");
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("static assets: disabled"));
}

#[test]
fn events_stream_uses_bearer_auth_cursor_and_emits_jsonl_only() {
    let directory = TestDirectory::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let token = "private-test-token";
    let credential = directory.path("reader.token");
    fs::write(&credential, format!("{token}\n")).unwrap();
    let server = thread::spawn(move || {
        let mut connection = accept_before(&listener, Instant::now() + Duration::from_secs(5));
        let request = read_headers(&mut connection);
        assert!(request.starts_with("GET /api/v1/events?after=uob%3Aevent%3A41 HTTP/1.1"));
        assert!(request.contains(&format!("authorization: Bearer {token}\r\n")));
        connection
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream; charset=utf-8\r\nconnection: close\r\n\r\ndata: {\"event_id\":\"event-42\",",
            )
            .unwrap();
        connection
            .write_all(b"\"kind\":\"transaction.started\"}\n\n")
            .unwrap();
    });

    let configuration = directory.path("events.toml");
    write_configuration(
        &configuration,
        "127.0.0.1:0".parse().unwrap(),
        &format!(
            "\n[events]\nendpoint = 'http://{address}/api/v1/events'\ncredentials_file = '{}'\n",
            credential.display()
        ),
    );
    let result = uob()
        .args(["events", "--config"])
        .arg(&configuration)
        .args(["--after", "uob:event:41", "--format", "jsonl"])
        .output()
        .expect("stream events");
    server.join().expect("event server");

    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&result.stdout).unwrap(),
        serde_json::json!({
            "event_id": "event-42",
            "kind": "transaction.started"
        })
    );
    assert!(result.stderr.is_empty());
    assert!(!String::from_utf8_lossy(&result.stdout).contains(token));
}

fn wait_until_listening(address: SocketAddr, child: &mut std::process::Child) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if TcpStream::connect(address).is_ok() {
            return;
        }
        assert!(
            child.try_wait().unwrap().is_none(),
            "service exited before listening"
        );
        assert!(Instant::now() < deadline, "service did not listen in time");
        thread::sleep(Duration::from_millis(20));
    }
}

fn http_get(address: SocketAddr, path: &str) -> String {
    let mut stream = TcpStream::connect(address).unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

fn read_headers(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 1024];
    while !bytes.ends_with(b"\r\n\r\n") {
        let count = stream.read(&mut buffer).unwrap();
        assert_ne!(count, 0, "request ended before headers");
        bytes.extend_from_slice(&buffer[..count]);
        assert!(bytes.len() <= 16 * 1024, "request headers too large");
    }
    String::from_utf8(bytes).unwrap()
}

fn accept_before(listener: &TcpListener, deadline: Instant) -> TcpStream {
    loop {
        match listener.accept() {
            Ok((connection, _)) => return connection,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "event client did not connect");
                thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!("accept event client: {error}"),
        }
    }
}
