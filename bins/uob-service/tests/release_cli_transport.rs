use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixListener,
    path::PathBuf,
    process::Command,
    thread,
};

struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn reject_response(response: Vec<u8>) {
    let directory =
        Directory(std::env::temp_dir().join(format!("uob-release-peer-{}", uuid::Uuid::new_v4())));
    fs::create_dir(&directory.0).unwrap();
    let socket = directory.0.join("control.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let peer = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut request = String::new();
        BufReader::new(&mut stream).read_line(&mut request).unwrap();
        // The peer may observe a broken pipe when the client enforces its frame bound.
        let _ = stream.write_all(&response);
    });
    let output = Command::new(env!("CARGO_BIN_EXE_uob"))
        .args(["release", "status", "--format", "json", "--socket"])
        .arg(&socket)
        .current_dir(&directory.0)
        .output()
        .unwrap();
    peer.join().unwrap();
    assert!(!output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"], "release_manager_protocol_error");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("peer-secret"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("peer-secret"));
}

#[test]
fn unknown_protocol_is_rejected_without_reflecting_response() {
    reject_response(
        br#"{"protocol":999,"manager_version":"peer-secret","code":"ok","status":{"sequence":0,"failed_operations":0}}
"#
        .to_vec(),
    );
}

#[test]
fn eof_does_not_turn_a_partial_frame_into_success() {
    reject_response(
        br#"{"protocol":1,"manager_version":"peer-secret","code":"ok","status":{"sequence":0,"failed_operations":0}}"#.to_vec(),
    );
}

#[test]
fn oversized_response_is_rejected_before_json_output() {
    let mut response =
        br#"{"protocol":1,"manager_version":"0.1.0","code":"ok","status":{"sequence":0,"failed_operations":0,"promotion":{"padding":"peer-secret"#
            .to_vec();
    response.resize(256 * 1024, b'x');
    response.extend_from_slice(b"\"}}}\n");
    reject_response(response);
}

#[test]
fn fifo_input_is_rejected_without_waiting_for_a_writer() {
    use rustix::fs::{CWD, Mode, mkfifoat};
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let directory =
        Directory(std::env::temp_dir().join(format!("uob-release-fifo-{}", uuid::Uuid::new_v4())));
    fs::create_dir(&directory.0).unwrap();
    mkfifoat(
        CWD,
        directory.0.join("manifest.json"),
        Mode::RUSR | Mode::WUSR,
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_uob"))
        .args(["release", "stage", "--bundle"])
        .arg(&directory.0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("release input blocked on a FIFO");
        }
        thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    let error: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(error["error"], "invalid_release_input");
}

#[test]
fn slow_preflight_returns_its_policy_result_not_a_transport_failure() {
    let directory =
        Directory(std::env::temp_dir().join(format!("uob-release-slow-{}", uuid::Uuid::new_v4())));
    fs::create_dir(&directory.0).unwrap();
    let socket = directory.0.join("control.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let peer = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut request = String::new();
        BufReader::new(&mut stream).read_line(&mut request).unwrap();
        // Request framing has a one-second limit; supervisor work is a separate phase.
        thread::sleep(std::time::Duration::from_millis(1200));
        writeln!(
            stream,
            r#"{{"protocol":1,"manager_version":"0.1.0","code":"preflight_rejected"}}"#
        )
        .unwrap();
    });
    let output = Command::new(env!("CARGO_BIN_EXE_uob"))
        .args([
            "release",
            "promote",
            "--release",
            &"a".repeat(64),
            "--socket",
        ])
        .arg(&socket)
        .output()
        .unwrap();
    peer.join().unwrap();
    assert!(!output.status.success());
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["code"], "preflight_rejected");
}
