#![cfg(unix)]

use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Process {
    child: Child,
    directory: PathBuf,
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn sigterm_drains_the_service_and_releases_the_listener_repeatedly() {
    for _ in 0..3 {
        let address = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let directory =
            std::env::temp_dir().join(format!("uob-lifecycle-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        let config = directory.join("bridge.toml");
        fs::write(&config, format!(
            "[bridge]\nid='lifecycle'\n[management]\nlisten_addr='{address}'\n[lifecycle]\nshutdown_timeout_seconds=1\n"
        )).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_uob"))
            .args(["serve", "--no-ui", "--config"])
            .arg(config)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut process = Process { child, directory };
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(mut stream) = TcpStream::connect(address) {
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                stream.write_all(b"GET /api/v1/identity HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").unwrap();
                let mut response = String::new();
                stream.read_to_string(&mut response).unwrap();
                assert!(response.starts_with("HTTP/1.1 200"));
                break;
            }
            assert!(process.child.try_wait().unwrap().is_none());
            assert!(Instant::now() < until, "startup deadline");
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            Command::new("kill")
                .args(["-TERM", &process.child.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        let until = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = process.child.try_wait().unwrap() {
                assert!(status.success(), "SIGTERM should exit cleanly: {status}");
                break;
            }
            assert!(Instant::now() < until, "shutdown deadline");
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            TcpListener::bind(address).is_ok(),
            "listener leaked after shutdown"
        );
    }
}

#[test]
fn packaged_service_uses_nonroot_identity_and_bounded_shutdown_and_journal() {
    let unit = include_str!("../../../packaging/systemd/uob.service");
    for directive in [
        "User=uob",
        "Group=uob",
        "KillMode=control-group",
        "TimeoutStopSec=25s",
        "SendSIGKILL=yes",
        "LogNamespace=uob",
        "NoNewPrivileges=yes",
        "StateDirectoryMode=0700",
    ] {
        assert!(
            unit.lines().any(|line| line == directive),
            "missing {directive}"
        );
    }
    let journal = include_str!("../../../packaging/systemd/journald-uob.conf");
    for directive in [
        "SystemMaxUse=32M",
        "RuntimeMaxUse=8M",
        "MaxRetentionSec=7day",
        "ForwardToSyslog=no",
    ] {
        assert!(journal.lines().any(|line| line == directive));
    }
}
