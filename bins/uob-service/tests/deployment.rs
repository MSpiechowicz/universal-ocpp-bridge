#![cfg(unix)]

use std::{
    fs,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
        process::CommandExt,
    },
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("uob-layout-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        for (source, name) in [
            (PathBuf::from(env!("CARGO_BIN_EXE_uob")), "uob"),
            (std::env::current_exe().unwrap(), "probe"),
        ] {
            fs::copy(source, root.join(name)).unwrap();
            fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self(fs::canonicalize(root).unwrap())
    }
    fn environment(&self, environment: &str, uid: Option<u32>) -> Instance {
        let root = self.0.join(environment);
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        for name in ["state", "run", "config", "state/export-spool"] {
            fs::create_dir(root.join(name)).unwrap();
            fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o700)).unwrap();
        }
        let address = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let sample = if environment == "production" {
            include_str!("../../../packaging/systemd/bridge.toml")
        } else {
            include_str!("../../../packaging/systemd/staging.toml")
        };
        let old = if environment == "production" {
            "127.0.0.1:8080"
        } else {
            "127.0.0.1:18080"
        };
        fs::write(
            root.join("config/bridge.toml"),
            sample.replace(old, &address.to_string()),
        )
        .unwrap();
        fs::write(root.join("config/secret"), b"synthetic-only").unwrap();
        if let Some(uid) = uid {
            assert!(
                Command::new("chown")
                    .arg("-R")
                    .arg(format!("{uid}:{uid}"))
                    .arg(&root)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        Instance {
            root,
            binary: self.0.join("uob"),
            environment: environment.to_owned(),
            address,
            uid,
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Instance {
    root: PathBuf,
    binary: PathBuf,
    environment: String,
    address: SocketAddr,
    uid: Option<u32>,
}
impl Instance {
    fn command(&self, verb: &str) -> Command {
        let mut command = Command::new(&self.binary);
        command.arg(verb);
        if verb == "config" {
            command.arg("check");
        }
        command
            .args(["--config"])
            .arg(self.root.join("config/bridge.toml"))
            .env("UOB_DEPLOYMENT_ENVIRONMENT", &self.environment)
            .env("STATE_DIRECTORY", self.root.join("state"))
            .env("RUNTIME_DIRECTORY", self.root.join("run"))
            .env("TOKIO_WORKER_THREADS", "2")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(uid) = self.uid {
            command.uid(uid).gid(uid);
        }
        command
    }
    fn start(&self) -> Process {
        let mut process = Process(self.command("serve").spawn().unwrap());
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(identity) = self.identity() {
                assert_eq!(identity["runtime"]["environment"], self.environment);
                break;
            }
            assert!(process.0.try_wait().unwrap().is_none(), "startup failed");
            assert!(Instant::now() < until, "startup deadline");
            thread::sleep(Duration::from_millis(20));
        }
        process
    }
    fn identity(&self) -> Option<serde_json::Value> {
        let mut socket =
            TcpStream::connect_timeout(&self.address, Duration::from_millis(100)).ok()?;
        socket
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        socket
            .write_all(
                b"GET /api/v1/identity HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        let mut response = String::new();
        socket.read_to_string(&mut response).unwrap();
        Some(serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap())
    }
}
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
impl Process {
    fn stop(&mut self) {
        assert!(
            Command::new("kill")
                .args(["-TERM", &self.0.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(Instant::now() < until);
            thread::sleep(Duration::from_millis(20));
        }
    }
}

fn deployment_layouts(uids: Option<(u32, u32)>) {
    let fixture = Fixture::new();
    let production = fixture.environment("production", uids.map(|ids| ids.0));
    assert!(production.command("config").status().unwrap().success());
    assert!(
        !production.root.join("state/identity.json").exists(),
        "offline check mutated state"
    );
    let mut prod = production.start(); // Staging absent, including on a production-only host.
    let staging = fixture.environment("staging", uids.map(|ids| ids.1));
    assert!(
        !staging
            .command("serve")
            .env("UOB_DEPLOYMENT_ENVIRONMENT", "production")
            .status()
            .unwrap()
            .success()
    );
    assert!(production.identity().is_some()); // Failed staging cannot stop production.
    let mut stage = staging.start();
    assert!(
        !production.command("serve").status().unwrap().success(),
        "duplicate ownership admitted"
    );
    for instance in [&production, &staging] {
        assert!(instance.root.join("state/operational.sqlite3").is_file());
        assert!(
            instance
                .root
                .join("state/operational.sqlite3-wal")
                .is_file()
        );
        assert!(instance.root.join("run/service.lock").is_file());
    }
    if let Some((prod_uid, stage_uid)) = uids {
        // Real pathname sockets under the same 0700 runtime boundary as future IPC.
        let _prod_socket = UnixListener::bind(production.root.join("run/control.sock")).unwrap();
        let _stage_socket = UnixListener::bind(staging.root.join("run/control.sock")).unwrap();
        for (uid, own, other) in [
            (prod_uid, &production, &staging),
            (stage_uid, &staging, &production),
        ] {
            assert!(
                Command::new(fixture.0.join("probe"))
                    .args(["--ignored", "--exact", "cross_user_probe"])
                    .env("UOB_PROBE_OWN", &own.root)
                    .env("UOB_PROBE_OTHER", &other.root)
                    .uid(uid)
                    .gid(uid)
                    .status()
                    .unwrap()
                    .success()
            );
        }
    }
    let before = production.identity().unwrap();
    stage.stop();
    assert!(production.identity().is_some());
    prod.stop();
    let mut restarted = production.start();
    let after = production.identity().unwrap();
    assert_eq!(before["bridge_id"], after["bridge_id"]);
    assert_ne!(
        before["runtime"]["process_instance_id"],
        after["runtime"]["process_instance_id"]
    );
    restarted.stop();
    // The same staging artifact and schema also boot with production absent on another host.
    let separate_host = Fixture::new();
    let standalone = separate_host.environment("staging", uids.map(|ids| ids.1));
    standalone.start().stop();
}

#[test]
fn real_processes_preserve_environment_and_independent_boot() {
    deployment_layouts(None);
}

#[test]
#[ignore = "requires root in a disposable Linux test environment; run by package CI"]
fn distinct_linux_users_cannot_write_peer_state_or_sockets() {
    deployment_layouts(Some((60001, 60002)));
}

#[test]
#[ignore = "child helper for the privileged isolation test"]
fn cross_user_probe() {
    let own = PathBuf::from(std::env::var_os("UOB_PROBE_OWN").expect("probe own"));
    let other = PathBuf::from(std::env::var_os("UOB_PROBE_OTHER").expect("probe peer"));
    fs::write(own.join("state/probe"), b"own").unwrap();
    for name in [
        "state/operational.sqlite3",
        "state/operational.sqlite3-wal",
        "state/identity.json",
        "state/export-spool/probe",
        "run/service.lock",
        "config/bridge.toml",
        "config/secret",
    ] {
        assert_eq!(
            fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .open(other.join(name))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::PermissionDenied,
            "{name}"
        );
    }
    assert_eq!(
        UnixStream::connect(other.join("run/control.sock"))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::PermissionDenied
    );
}

#[test]
fn package_has_distinct_users_paths_slices_and_no_staging_boot_dependency() {
    let production = include_str!("../../../packaging/systemd/uob.service");
    let staging = include_str!("../../../packaging/systemd/uob-staging.service");
    for (unit, user, environment, peer) in [
        (production, "uob", "production", "uob-staging"),
        (staging, "uob-staging", "staging", "uob"),
    ] {
        for directive in [
            format!("User={user}"),
            format!("Group={user}"),
            format!("Environment=UOB_DEPLOYMENT_ENVIRONMENT={environment}"),
            format!("Slice=uob-{environment}.slice"),
            format!("StateDirectory={user}"),
            format!("RuntimeDirectory={user}"),
            format!("LogNamespace={user}"),
            format!("InaccessiblePaths=-/etc/{peer} -/var/lib/{peer} -/run/{peer}"),
            "ProtectSystem=strict".to_owned(),
            "RuntimeDirectoryMode=0700".to_owned(),
            "StateDirectoryMode=0700".to_owned(),
            "UMask=0077".to_owned(),
        ] {
            assert!(unit.lines().any(|line| line == directive), "{directive}");
        }
        for line in unit.lines().filter(|line| {
            ["Requires=", "Wants=", "After=", "BindsTo=", "PartOf="]
                .iter()
                .any(|prefix| line.starts_with(prefix))
        }) {
            assert!(!line.contains(peer), "cross-environment dependency {line}");
        }
    }
    assert!(production.contains("WantedBy=multi-user.target"));
    assert!(!staging.contains("WantedBy="), "staging must be opt-in");
    assert!(
        include_str!("../../../packaging/systemd/uob-staging-sysusers.conf")
            .starts_with("u uob-staging ")
    );
}
