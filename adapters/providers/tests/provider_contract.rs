//! Reusable artifact and PKI provider contracts, run against the local test providers.
#![cfg(unix)]

#[path = "provider_contract/artifacts.rs"]
mod artifacts;
#[path = "provider_contract/contract.rs"]
mod contract;
#[path = "provider_contract/environment.rs"]
mod environment;
#[path = "provider_contract/network.rs"]
mod network;
#[path = "provider_contract/pki.rs"]
mod pki;

use std::{fs, net::SocketAddr, path::PathBuf, time::Duration};

use time::OffsetDateTime;
use tokio::{net::TcpListener, sync::oneshot};
use uob_application::{RuntimeResourceBudget, RuntimeResourceLimits, RuntimeSecurityPolicy};
use uob_contracts::{Environment, UtcTimestamp};
use uob_provider_adapter::{
    artifacts::{ArtifactTransfers, TransferLimits},
    test_artifacts::{TestArtifactConfiguration, TestArtifactService},
    test_ca::TestCertificateAuthority,
};

const ORGANIZATION: &str = "UOB Contract CSO";

fn demo() -> RuntimeSecurityPolicy {
    RuntimeSecurityPolicy::new(Environment::Demo)
}

fn now() -> UtcTimestamp {
    UtcTimestamp::new(OffsetDateTime::now_utc())
}

fn authority() -> TestCertificateAuthority {
    TestCertificateAuthority::generate(demo(), ORGANIZATION).unwrap()
}

fn transfers(maximum_artifact_bytes: u64, timeout: Duration) -> ArtifactTransfers {
    let budget = RuntimeResourceBudget::new(RuntimeResourceLimits::default()).unwrap();
    ArtifactTransfers::new(
        budget,
        TransferLimits {
            buffer_bytes: 16 * 1024,
            maximum_artifact_bytes,
            timeout,
        },
    )
    .unwrap()
}

/// Private spool directory removed when the test ends.
struct Spool(PathBuf);

impl Spool {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("uob-provider-contract-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn is_empty(&self) -> bool {
        fs::read_dir(&self.0).unwrap().next().is_none()
    }
}

impl Drop for Spool {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn configuration(public_base: &str, spool: &Spool) -> TestArtifactConfiguration {
    TestArtifactConfiguration {
        public_base: public_base.to_owned(),
        spool_directory: spool.0.clone(),
        maximum_artifacts: 4,
        maximum_uploads: 4,
    }
}

/// Artifact service bound to a loopback port and served until dropped.
struct Served {
    service: TestArtifactService,
    address: SocketAddr,
    spool: Spool,
    _shutdown: oneshot::Sender<()>,
}

async fn serve(maximum_artifact_bytes: u64, timeout: Duration) -> Served {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let spool = Spool::new();
    let service = TestArtifactService::new(
        demo(),
        transfers(maximum_artifact_bytes, timeout),
        configuration(&format!("http://{address}"), &spool),
    )
    .unwrap();
    let (shutdown, stopped) = oneshot::channel();
    let server = service.clone();
    tokio::spawn(async move {
        server
            .serve(listener, async move {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    Served {
        service,
        address,
        spool,
        _shutdown: shutdown,
    }
}

fn firmware(size: usize) -> Vec<u8> {
    (0..size)
        .map(|index| u8::try_from(index % 251).unwrap())
        .collect()
}
