mod control_support;
use control_support::{Fixture, TOKEN, control_document, wait_scenario};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn serve_requires_explicit_configuration_and_bind_and_rejects_production() {
    let fixture = Fixture::new(&wait_scenario("demo-alpha", 1));
    for args in [
        vec!["serve"],
        vec!["serve", "--control-bind", "127.0.0.1:9001"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_uob-sim"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
    }
    fixture.write("control.toml", &control_document("production"));
    let output = Command::new(env!("CARGO_BIN_EXE_uob-sim"))
        .args(["serve", "--config"])
        .arg(fixture.directory.join("control.toml"))
        .args(["--control-bind", "127.0.0.1:9001"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("explicit_test_environment_required"));
}

#[test]
fn explicit_serve_command_exposes_authenticated_catalog_on_real_http_socket() {
    let fixture = Fixture::new(&wait_scenario("demo-alpha", 1));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let mut process = Process(
        Command::new(env!("CARGO_BIN_EXE_uob-sim"))
            .args(["serve", "--config"])
            .arg(fixture.directory.join("control.toml"))
            .arg("--control-bind")
            .arg(address.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut socket = loop {
        assert!(
            process.0.try_wait().unwrap().is_none(),
            "server exited before binding"
        );
        if let Ok(socket) = TcpStream::connect(address) {
            break socket;
        }
        assert!(Instant::now() < deadline, "server never bound");
        std::thread::sleep(Duration::from_millis(10));
    };
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    write!(socket, "GET /api/v1/scenarios HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {TOKEN}\r\nConnection: close\r\n\r\n").unwrap();
    let mut response = String::new();
    socket.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("\"environment\":\"demo\""));
    assert!(response.contains("no-store"));
}
