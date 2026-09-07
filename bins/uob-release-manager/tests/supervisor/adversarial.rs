use super::{Fixture, grant};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{
        fs::{PermissionsExt, symlink},
        net::UnixStream,
    },
    time::{Duration, Instant},
};
use uob_release_manager::supervisor::{
    Code, Permission, Request, Response, Supervisor, ipc::Server,
};

#[test]
fn ipc_rejects_forged_identity_paths_units_shell_and_supervisor_replacement() {
    let f = Fixture::new();
    let daemon = f.launch(&[Permission::Read, Permission::Stage, Permission::Activate]);
    for input in [
        r#"{"operation":"status","uid":0}"#,
        r#"{"operation":"stage","digest":"../../etc/shadow"}"#,
        r#"{"operation":"stage","digest":"$(touch /tmp/pwned)"}"#,
        r#"{"operation":"promote","digest":"a","unit":"other.service"}"#,
        r#"{"operation":"rollback","unit":"uob-release-manager.service"}"#,
        r#"{"operation":"replace_supervisor"}"#,
        r#"{"operation":"execute","command":"/bin/sh"}"#,
        r#"{"operation":"stage","path":"/usr/local/libexec/uob-release-manager"}"#,
    ] {
        assert_eq!(daemon.raw(input).code, Code::InvalidRequest, "{input}");
    }
    assert!(!f.state.join("state.json").exists());
    assert_eq!(
        daemon
            .raw(r#"{"operation":"status"}"#)
            .status
            .unwrap()
            .sequence,
        0
    );
}

#[test]
fn kernel_uid_cannot_be_selected_by_the_requester() {
    let f = Fixture::new();
    let daemon = f.launch(&[Permission::Stage]);
    assert_eq!(
        daemon.raw(r#"{"operation":"status"}"#).code,
        Code::Forbidden
    );
    assert_eq!(
        daemon.raw(r#"{"operation":"rollback"}"#).code,
        Code::Forbidden
    );
    assert_eq!(
        daemon
            .raw(r#"{"operation":"status","permissions":["read"]}"#)
            .code,
        Code::InvalidRequest
    );
    assert_eq!(
        daemon
            .raw(&format!(
                r#"{{"operation":"stage","digest":"{}"}}"#,
                f.artifacts.digest()
            ))
            .code,
        Code::Ok
    );
}

#[test]
fn competing_owners_unsafe_state_and_stale_non_socket_paths_fail_closed() {
    let f = Fixture::new();
    let grants = || vec![grant(10, &[Permission::Read])];
    let manager = f.manager(grants());
    assert!(Supervisor::open(&f.state, &f.store, f.artifacts.policy.clone(), grants()).is_err());
    let server = Server::bind(&f.runtime).unwrap();
    assert!(Server::bind(&f.runtime).is_err());
    drop(server);
    fs::write(f.runtime.join("control.sock"), b"do not delete").unwrap();
    assert!(Server::bind(&f.runtime).is_err());
    assert_eq!(
        fs::read(f.runtime.join("control.sock")).unwrap(),
        b"do not delete"
    );
    drop(manager);
    fs::set_permissions(&f.state, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(Supervisor::open(&f.state, &f.store, f.artifacts.policy.clone(), grants()).is_err());
    fs::set_permissions(&f.state, fs::Permissions::from_mode(0o700)).unwrap();
    let outside = f.artifacts.root.join("outside.json");
    fs::write(&outside, b"{}").unwrap();
    symlink(&outside, f.state.join("state.json")).unwrap();
    assert!(Supervisor::open(&f.state, &f.store, f.artifacts.policy.clone(), grants()).is_err());
    assert_eq!(fs::read(outside).unwrap(), b"{}");
}

#[test]
fn interrupted_publication_keeps_status_available_and_blocks_more_operations() {
    let f = Fixture::new();
    let grants = || vec![grant(10, &[Permission::Read, Permission::Stage])];
    {
        let mut manager = f.manager(grants());
        assert_eq!(
            manager
                .handle(
                    10,
                    Request::Stage {
                        digest: f.artifacts.digest().into()
                    }
                )
                .code,
            Code::Ok
        );
    }
    fs::write(f.state.join("state.next"), b"partial").unwrap();
    let mut manager = f.manager(grants());
    let status = manager.handle(10, Request::Status {});
    assert_eq!(status.code, Code::RecoveryRequired);
    assert_eq!(status.status.unwrap().sequence, 1);
    assert_eq!(
        manager
            .handle(
                10,
                Request::Stage {
                    digest: f.artifacts.digest().into()
                }
            )
            .code,
        Code::RecoveryRequired
    );
    assert_eq!(fs::read(f.state.join("state.next")).unwrap(), b"partial");
}

#[test]
fn slow_and_oversized_peers_are_bounded_and_status_recovers() {
    let f = Fixture::new();
    let daemon = f.launch(&[Permission::Read]);
    let mut socket = UnixStream::connect(&daemon.socket).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    socket.write_all(&vec![b'x'; 1025]).unwrap();
    let mut response = String::new();
    BufReader::new(socket).read_line(&mut response).unwrap();
    assert_eq!(
        serde_json::from_str::<Response>(&response).unwrap().code,
        Code::InvalidRequest
    );
    let mut socket = UnixStream::connect(&daemon.socket).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let started = Instant::now();
    socket.write_all(b"{").unwrap();
    std::thread::sleep(Duration::from_millis(600));
    socket.write_all(b" ").unwrap();
    let mut response = String::new();
    BufReader::new(socket).read_line(&mut response).unwrap();
    assert_eq!(
        serde_json::from_str::<Response>(&response).unwrap().code,
        Code::InvalidRequest
    );
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "trickling reset deadline"
    );
    assert_eq!(daemon.raw(r#"{"operation":"status"}"#).code, Code::Ok);
}
