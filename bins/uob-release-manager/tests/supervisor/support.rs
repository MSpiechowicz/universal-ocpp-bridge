#[allow(dead_code)]
#[path = "../artifact_store/support.rs"]
mod artifacts;
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Mutex, MutexGuard},
    time::{Duration, Instant},
};
use uob_release_manager::{
    artifacts::ArtifactStore,
    supervisor::{Grant, Permission, Response, Supervisor},
};

// Fork/exec briefly inherits other threads' open flock descriptions. Serialize
// process-owning fixtures so unrelated test locks cannot survive a scoped drop.
static FIXTURE_LOCK: Mutex<()> = Mutex::new(());

pub struct Fixture {
    _guard: MutexGuard<'static, ()>,
    pub artifacts: artifacts::Fixture,
    pub store: PathBuf,
    pub state: PathBuf,
    pub runtime: PathBuf,
}

impl Fixture {
    pub fn new() -> Self {
        let guard = FIXTURE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let artifacts = artifacts::Fixture::new();
        let store = artifacts.root.join("store");
        let state = artifacts.root.join("state");
        let runtime = artifacts.root.join("run");
        for path in [&store, &state, &runtime] {
            fs::create_dir(path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let f = Self {
            _guard: guard,
            artifacts,
            store,
            state,
            runtime,
        };
        let (manifest, signature) = f.artifacts.signed();
        ArtifactStore::open(&f.store)
            .unwrap()
            .install(
                &manifest,
                &signature,
                &mut f.artifacts.payload.as_slice(),
                &f.artifacts.policy,
            )
            .unwrap();
        f
    }

    pub fn manager(&self, grants: Vec<Grant>) -> Supervisor {
        Supervisor::open(
            &self.state,
            &self.store,
            self.artifacts.policy.clone(),
            grants,
        )
        .unwrap()
    }

    pub fn launch(&self, permissions: &[Permission]) -> Daemon {
        let input = serde_json::json!({
            "store": self.store,
            "state": self.state,
            "runtime": self.runtime,
            "policy": self.artifacts.policy,
            "grants": [{"uid": rustix::process::geteuid().as_raw(), "permissions": permissions}]
        });
        let input_path = self.artifacts.root.join("daemon.json");
        fs::write(&input_path, serde_json::to_vec(&input).unwrap()).unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "daemon_fixture", "--ignored"])
            .env("UOB_SUPERVISOR_TEST_INPUT", &input_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut daemon = Daemon {
            child,
            socket: self.runtime.join("control.sock"),
        };
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(
                daemon.child.try_wait().unwrap().is_none(),
                "daemon exited before bind"
            );
            assert!(Instant::now() < end, "daemon did not bind");
            if let Ok(mut socket) = UnixStream::connect(&daemon.socket) {
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                writeln!(socket, "{{\"operation\":\"status\"}}").unwrap();
                let mut response = String::new();
                BufReader::new(socket).read_line(&mut response).unwrap();
                serde_json::from_str::<Response>(&response).unwrap();
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        daemon
    }
}

pub struct Daemon {
    child: Child,
    pub socket: PathBuf,
}
impl Daemon {
    pub fn raw(&self, value: &str) -> Response {
        let mut socket = UnixStream::connect(&self.socket).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        writeln!(socket, "{value}").unwrap();
        let mut response = String::new();
        BufReader::new(socket).read_line(&mut response).unwrap();
        serde_json::from_str(&response).unwrap()
    }
}
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
