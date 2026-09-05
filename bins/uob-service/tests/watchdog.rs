#![cfg(unix)]

use std::{
    fs,
    net::TcpListener,
    os::unix::{fs::PermissionsExt, net::UnixDatagram},
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
};

struct Service {
    child: Child,
    root: PathBuf,
    notifications: UnixDatagram,
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl Service {
    fn start(configuration: &str) -> Self {
        Self::start_with(configuration, false)
    }

    fn start_with(configuration: &str, corrupt_storage: bool) -> Self {
        let root = std::env::temp_dir().join(format!("uob-watchdog-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        for name in ["state", "runtime"] {
            fs::create_dir(root.join(name)).unwrap();
            fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o700)).unwrap();
        }
        if corrupt_storage {
            fs::write(root.join("state/operational.sqlite3"), b"unbound storage").unwrap();
        }
        fs::write(root.join("bridge.toml"), configuration).unwrap();
        let notifications = UnixDatagram::bind(root.join("notify")).unwrap();
        notifications
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_uob"))
            .args(["serve", "--no-ui", "--config"])
            .arg(root.join("bridge.toml"))
            .env("NOTIFY_SOCKET", root.join("notify"))
            .env("WATCHDOG_USEC", "200000")
            .env_remove("WATCHDOG_PID")
            .env("UOB_DEPLOYMENT_ENVIRONMENT", "production")
            .env("STATE_DIRECTORY", root.join("state"))
            .env("RUNTIME_DIRECTORY", root.join("runtime"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        Self {
            child,
            root,
            notifications,
        }
    }

    fn receive(&self) -> String {
        let mut buffer = [0; 256];
        let count = self.notifications.recv(&mut buffer).unwrap();
        String::from_utf8(buffer[..count].to_vec()).unwrap()
    }

    fn signal(&self, signal: &str) {
        assert!(
            Command::new("kill")
                .args([signal, &self.child.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
    }
}

#[test]
fn ready_follows_database_and_bind_and_progress_stops_with_the_process() {
    let mut service =
        Service::start("[bridge]\nid='watchdog'\n[management]\nlisten_addr='127.0.0.1:0'\n");
    assert!(service.receive().starts_with("READY=1\n"));
    assert!(service.root.join("state/operational.sqlite3").exists());
    assert_eq!(service.receive(), "WATCHDOG=1");
    service.signal("-STOP");
    // Drain any notification sent concurrently with SIGSTOP. The receiving test
    // thread keeps running, demonstrating that wall-clock progress is insufficient.
    service
        .notifications
        .set_read_timeout(Some(Duration::from_millis(300)))
        .unwrap();
    let mut buffer = [0; 256];
    while service.notifications.recv(&mut buffer).is_ok() {}
    assert!(service.notifications.recv(&mut buffer).is_err());
    service.signal("-CONT");
    service
        .notifications
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    assert_eq!(service.receive(), "WATCHDOG=1");
    service.signal("-TERM");
    loop {
        let message = service.receive();
        if message != "WATCHDOG=1" {
            assert!(message.starts_with("STOPPING=1\n"));
            break;
        }
    }
    assert!(service.child.wait().unwrap().success());
}

#[test]
fn invalid_configuration_and_bind_failure_never_notify_ready() {
    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    let bind_failure = format!(
        "[bridge]\nid='watchdog'\n[management]\nlisten_addr='{}'\n",
        occupied.local_addr().unwrap()
    );
    for config in ["invalid TOML", &bind_failure] {
        let mut service = Service::start(config);
        assert!(!service.child.wait().unwrap().success());
        service
            .notifications
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let mut buffer = [0; 256];
        while let Ok(count) = service.notifications.recv(&mut buffer) {
            assert!(!String::from_utf8_lossy(&buffer[..count]).contains("READY=1"));
        }
    }
}

#[test]
fn both_units_bound_restarts_and_preserve_per_invocation_termination_causes() {
    for unit in [
        include_str!("../../../packaging/systemd/uob.service"),
        include_str!("../../../packaging/systemd/uob-staging.service"),
    ] {
        for directive in [
            "Type=notify",
            "NotifyAccess=main",
            "WatchdogSec=10s",
            "TimeoutStartSec=30s",
            "TimeoutAbortSec=5s",
            "Restart=on-failure",
            "RestartSec=5s",
            "StartLimitIntervalSec=60",
            "StartLimitBurst=5",
            "${SERVICE_RESULT}",
            "${EXIT_CODE}",
            "${EXIT_STATUS}",
            "${INVOCATION_ID}",
        ] {
            assert!(
                unit.lines().any(|line| line.contains(directive)),
                "missing {directive}"
            );
        }
    }
}

#[test]
fn failed_storage_initialization_never_notifies_ready() {
    let mut service = Service::start_with("[bridge]\nid='watchdog'\n", true);
    assert!(!service.child.wait().unwrap().success());
    service
        .notifications
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    assert!(service.notifications.recv(&mut [0; 256]).is_err());
}
