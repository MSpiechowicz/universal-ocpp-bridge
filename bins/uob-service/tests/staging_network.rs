#![cfg(target_os = "linux")]

use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::{fs::PermissionsExt, process::CommandExt},
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
fn shipped_network_policy_is_required_and_cannot_be_changed_by_staging() {
    let service = include_str!("../../../packaging/systemd/uob-staging.service");
    for directive in [
        "Requires=uob-staging-network.service",
        "BindsTo=uob-staging-network.service",
        "NetworkNamespacePath=/run/netns/uob-staging",
        "RestrictNamespaces=yes",
        "CapabilityBoundingSet=",
        "NoNewPrivileges=yes",
    ] {
        assert!(service.lines().any(|line| line == directive), "{directive}");
    }
}

#[test]
#[ignore = "requires root and a disposable namespace; scripts/test-staging-network.sh"]
fn isolated_peers_work_and_host_sockets_are_unreachable() {
    let root = std::env::temp_dir().join(format!("uob-network-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
    for (source, name) in [
        (std::env::current_exe().unwrap(), "probe"),
        (env!("CARGO_BIN_EXE_uob").into(), "uob"),
    ] {
        fs::copy(source, root.join(name)).unwrap();
        fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::write(
        root.join("staging.toml"),
        include_str!("../../../packaging/systemd/staging.toml"),
    )
    .unwrap();
    let mut host = Command::new(root.join("uob"));
    let result = host
        .args(["serve", "--config"])
        .arg(root.join("staging.toml"))
        .uid(60002)
        .gid(60002)
        .output()
        .unwrap();
    assert!(
        !result.status.success(),
        "staging started on the host network"
    );
    assert!(String::from_utf8_lossy(&result.stderr).contains("isolated /run/netns"));

    // Synthetic production control, shared broker, and management listeners on host loopback.
    let control = TcpListener::bind("127.0.0.1:0").unwrap();
    let broker = TcpListener::bind("127.0.0.1:18883").unwrap();
    let management = TcpListener::bind("127.0.0.1:18080").unwrap();
    for listener in [&control, &broker, &management] {
        listener.set_nonblocking(true).unwrap();
    }
    let status = Command::new("ip")
        .args(["netns", "exec", "uob-staging"])
        .arg(root.join("probe"))
        .args(["--ignored", "--exact", "namespace_probe"])
        .env("UOB_NETWORK_FIXTURE", &root)
        .env(
            "UOB_HOST_CONTROL",
            control.local_addr().unwrap().to_string(),
        )
        .status()
        .unwrap();
    assert!(status.success());
    for listener in [&control, &broker, &management] {
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
    // Misconfigured uplinks are rejected by the daemon even inside the expected namespace.
    assert!(
        Command::new("ip")
            .args([
                "-n",
                "uob-staging",
                "link",
                "add",
                "unexpected",
                "type",
                "dummy"
            ])
            .status()
            .unwrap()
            .success()
    );
    let result = Command::new("ip")
        .args(["netns", "exec", "uob-staging"])
        .arg(root.join("uob"))
        .args(["serve", "--config"])
        .arg(root.join("staging.toml"))
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("isolated /run/netns"));
    assert!(
        Command::new("ip")
            .args(["-n", "uob-staging", "link", "del", "unexpected"])
            .status()
            .unwrap()
            .success()
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "privileged harness child; no direct invocation"]
fn namespace_probe() {
    let root = std::path::PathBuf::from(std::env::var_os("UOB_NETWORK_FIXTURE").unwrap());
    for address in [
        std::env::var("UOB_HOST_CONTROL").unwrap(),
        "127.0.0.1:18883".to_owned(),
        "127.0.0.1:18080".to_owned(),
        "192.0.2.1:443".to_owned(),
        "[2001:db8::1]:443".to_owned(),
    ] {
        assert!(
            TcpStream::connect_timeout(&address.parse().unwrap(), Duration::from_millis(200))
                .is_err(),
            "escaped to {address}"
        );
    }
    // Test peers explicitly placed inside the namespace can communicate over real sockets.
    let listener = TcpListener::bind("127.0.0.1:18883").unwrap();
    let mut sender = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    sender.write_all(b"synthetic-test-peer").unwrap();
    let (mut receiver, _) = listener.accept().unwrap();
    receiver
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut payload = [0; 19];
    receiver.read_exact(&mut payload).unwrap();
    assert_eq!(&payload, b"synthetic-test-peer");

    // Run the actual daemon as the unprivileged staging UID in the namespace.
    let mut child = Process(
        Command::new(root.join("uob"))
            .args(["serve", "--config"])
            .arg(root.join("staging.toml"))
            .env("TOKIO_WORKER_THREADS", "2")
            .uid(60002)
            .gid(60002)
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut socket = loop {
        if let Ok(socket) = TcpStream::connect("127.0.0.1:18080") {
            break socket;
        }
        assert!(child.0.try_wait().unwrap().is_none(), "staging exited");
        assert!(Instant::now() < deadline, "staging startup deadline");
        std::thread::sleep(Duration::from_millis(20));
    };
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    socket
        .write_all(b"GET /api/v1/identity HTTP/1.0\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    socket.read_to_string(&mut response).unwrap();
    assert!(response.contains("200 OK") && response.contains("staging"));
}
