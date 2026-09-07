//! Bounded Linux Unix-socket transport with kernel-authenticated peer UIDs.
use super::{Code, Request, Response, Supervisor};
use crate::artifacts::{InstallError, filesystem as disk};
use std::{
    fs::{self, File, Permissions},
    io::{self, Read, Write},
    os::unix::{
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const MAX_REQUEST: usize = 1024;
const DEADLINE: Duration = Duration::from_secs(1);
const MAX_CLIENTS: usize = 4;

/// Owns the socket and its lock independently of the bridge process.
pub struct Server {
    listener: UnixListener,
    path: PathBuf,
    _lock: File,
}

impl Server {
    /// Binds `control.sock` beneath a canonical owner-controlled runtime directory.
    ///
    /// # Errors
    /// Rejects unsafe paths, non-socket stale entries and competing listeners.
    pub fn bind(runtime: &Path) -> Result<Self, InstallError> {
        disk::directory(runtime)?;
        if fs::metadata(runtime)?.mode() & 0o007 != 0 {
            return Err(InstallError::Rejected(
                "IPC directory must exclude other users",
            ));
        }
        let lock_path = runtime.join("ipc.lock");
        let lock = match disk::open(&lock_path, true, true) {
            Ok(file) => file,
            Err(InstallError::Io(e)) if e.kind() == io::ErrorKind::AlreadyExists => {
                disk::open(&lock_path, true, false)?
            }
            Err(error) => return Err(error),
        };
        lock.try_lock()
            .map_err(|_| InstallError::Rejected("IPC owner already running"))?;
        let path = runtime.join("control.sock");
        match fs::symlink_metadata(&path) {
            Ok(meta) => {
                if !meta.file_type().is_socket()
                    || meta.uid() != rustix::process::geteuid().as_raw()
                    || UnixStream::connect(&path).is_ok()
                {
                    return Err(InstallError::Rejected("unsafe or active IPC socket"));
                }
                fs::remove_file(&path)?;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
        let listener = UnixListener::bind(&path)?;
        fs::set_permissions(&path, Permissions::from_mode(0o660))?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            path,
            _lock: lock,
        })
    }

    /// Runs a fixed-capacity connection loop. Peers cannot allocate unbounded workers.
    /// Read deadlines are absolute, so trickling bytes cannot extend a connection.
    ///
    /// # Errors
    /// Returns on listener failure; systemd independently applies bounded restart.
    pub fn run(&self, supervisor: Supervisor) -> io::Result<()> {
        let shared = Arc::new(Mutex::new(supervisor));
        let active = Arc::new(AtomicUsize::new(0));
        loop {
            match self.listener.accept() {
                Ok((mut stream, _)) => {
                    if active.load(Ordering::Acquire) >= MAX_CLIENTS {
                        stream.set_write_timeout(Some(Duration::from_millis(10)))?;
                        let _ = respond(&mut stream, &Response::code(Code::Busy));
                        continue;
                    }
                    active.fetch_add(1, Ordering::AcqRel);
                    let count = Arc::clone(&active);
                    let manager = Arc::clone(&shared);
                    let spawned = std::thread::Builder::new()
                        .name("release-ipc".into())
                        .spawn(move || {
                            let _slot = Slot(count);
                            let response = serve(&mut stream, &manager);
                            let _ = stream.set_write_timeout(Some(DEADLINE));
                            let _ = respond(&mut stream, &response);
                        });
                    if spawned.is_err() {
                        active.fetch_sub(1, Ordering::AcqRel);
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => (),
                Err(error) => return Err(error),
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

struct Slot(Arc<AtomicUsize>);
impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn serve(stream: &mut UnixStream, supervisor: &Mutex<Supervisor>) -> Response {
    let Ok(peer) = rustix::net::sockopt::socket_peercred(&*stream) else {
        return Response::code(Code::Forbidden);
    };
    let Ok(request) = read_request(stream) else {
        return Response::code(Code::InvalidRequest);
    };
    let Ok(mut supervisor) = supervisor.try_lock() else {
        return Response::code(Code::Busy);
    };
    supervisor.handle(peer.uid.as_raw(), request)
}

fn read_request(stream: &mut UnixStream) -> io::Result<Request> {
    let end = Instant::now() + DEADLINE;
    let mut bytes = Vec::with_capacity(MAX_REQUEST);
    loop {
        let left = end
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or(io::ErrorKind::TimedOut)?;
        stream.set_read_timeout(Some(left))?;
        let mut byte = [0];
        stream.read_exact(&mut byte)?;
        if byte[0] == b'\n' {
            break;
        }
        if bytes.len() == MAX_REQUEST {
            return Err(io::ErrorKind::InvalidData.into());
        }
        bytes.push(byte[0]);
    }
    serde_json::from_slice(&bytes).map_err(|_| io::ErrorKind::InvalidData.into())
}

fn respond(stream: &mut UnixStream, response: &Response) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(response)?;
    bytes.push(b'\n');
    stream.write_all(&bytes)
}
