//! Opt-in demo station runtime; one private store and one bounded authenticated socket owner.
mod files;
mod runtime;

use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    sync::Arc,
};
use tokio::net::TcpListener;
use uob_application::{Application, LocalAuthorizationService, PageLimit, StationEvent};
use uob_contracts::{Environment, ResourceRef, StationId, TargetInstanceId, TransactionSnapshot};
use uob_protocol_adapter::{
    OcppEndpoint, StationAuthenticator, StationCredential, StationRegistration,
};
use uob_storage_adapter::{DEFAULT_WORK_QUEUE_CAPACITY, SqliteOperationalStore};

use crate::configuration::charging::ValidatedChargingConfiguration;

pub(crate) type ChargingStore =
    SqliteOperationalStore<Value, StationEvent, TransactionSnapshot, String>;
pub(crate) type ChargingAuthorization =
    LocalAuthorizationService<Value, StationEvent, TransactionSnapshot, String>;

/// Shared source of truth and explicitly scoped read credential for later management wiring.
pub(crate) struct ChargingState {
    pub(crate) store: ChargingStore,
    pub(crate) roster: Vec<ResourceRef>,
    pub(crate) read_grant: files::ReadGrant,
    pub(crate) authorization: Arc<ChargingAuthorization>,
    _directory: File,
    _lock: File,
}

pub(crate) struct ChargingRuntime {
    pub(crate) state: ChargingState,
    listener: TcpListener,
    endpoint: OcppEndpoint,
    receiver: uob_protocol_adapter::StationConnectionReceiver,
    resources: BTreeMap<StationId, Vec<ResourceRef>>,
    target: Option<(TargetInstanceId, u64)>,
}

impl ChargingRuntime {
    pub(crate) async fn open(
        config: ValidatedChargingConfiguration,
        application: &Application,
        target: Option<(TargetInstanceId, u64)>,
    ) -> io::Result<Self> {
        let fail = io::Error::other;
        if application.runtime_identity().environment != Environment::Demo
            || !config.listen_addr.ip().is_loopback()
        {
            return Err(fail("unsafe charging transport environment"));
        }
        let directory = files::directory(&config.state_directory).map_err(fail)?;
        let lock = files::state_lock(&config.state_directory).map_err(fail)?;
        let mut seen = BTreeSet::new();
        let grant_path = PathBuf::from(config.read_grant_file.as_str());
        let mut grant = files::secret(&grant_path, &mut seen).map_err(fail)?;
        if !std::str::from_utf8(&grant).is_ok_and(|token| {
            uob_management_adapter::token_matches_environment(token, Environment::Demo)
        }) {
            grant.fill(0);
            return Err(fail("charging read grant audience invalid"));
        }
        let read_grant = files::grant(grant);
        let mut registrations = Vec::with_capacity(config.stations.len());
        let mut resources = BTreeMap::new();
        let mut roster = Vec::new();
        for station in config.stations {
            let path = PathBuf::from(station.credential_file.as_str());
            let mut secret = files::secret(&path, &mut seen).map_err(fail)?;
            let credential = StationCredential::from_secret(&secret);
            secret.fill(0);
            let credential = credential.map_err(io::Error::other)?;
            registrations.push((
                StationRegistration {
                    station_id: station.station_id.clone(),
                    credential: station.credential_file,
                    client_certificate: None,
                },
                station.protocol,
                credential,
            ));
            roster.push(station.resources[0].clone());
            resources.insert(station.station_id, station.resources);
        }
        let capacity = resources.len();
        let authenticator =
            StationAuthenticator::demo_with_protocols(registrations).map_err(io::Error::other)?;
        let database = prepare_database(
            &config.state_directory,
            application.identity().bridge_id.as_str(),
            &directory,
        )?;
        let store: ChargingStore =
            SqliteOperationalStore::open(&database, DEFAULT_WORK_QUEUE_CAPACITY)
                .map_err(io::Error::other)?;
        files::check_database_files(&config.state_directory).map_err(fail)?;
        let authorization = Arc::new(
            ChargingAuthorization::recover(
                Arc::new(store.clone()),
                PageLimit::new(100).map_err(io::Error::other)?,
            )
            .await
            .map_err(io::Error::other)?,
        );
        runtime::reconcile(&store, &resources, application.identity()).await?;
        let (endpoint, receiver) =
            OcppEndpoint::new(authenticator, application, capacity).map_err(io::Error::other)?;
        let listener = TcpListener::bind(config.listen_addr).await?;
        Ok(Self {
            state: ChargingState {
                store,
                roster,
                read_grant,
                authorization,
                _directory: directory,
                _lock: lock,
            },
            listener,
            endpoint,
            receiver,
            resources,
            target,
        })
    }

    pub(crate) async fn serve(
        self,
        application: Application,
        stop: impl Future<Output = ()>,
    ) -> io::Result<()> {
        runtime::serve(self, application, stop).await
    }
}

fn prepare_database(
    state: &std::path::Path,
    bridge: &str,
    directory: &File,
) -> io::Result<PathBuf> {
    files::check_database_files(state).map_err(io::Error::other)?;
    let database = state.join("charging.sqlite3");
    if !database.exists()
        && ["charging.sqlite3-wal", "charging.sqlite3-shm"]
            .iter()
            .any(|name| state.join(name).exists())
    {
        return Err(io::Error::other("charging state identity mismatch"));
    }
    // A crash before DB creation leaves a bridge-bound marker that can be resumed.
    bind_identity(state, bridge, database.exists())?;
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&database)
    {
        Ok(file) => {
            file.sync_all()?;
            directory.sync_all()?;
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error),
    }
    files::check_database_files(state).map_err(io::Error::other)?;
    Ok(database)
}

fn bind_identity(dir: &std::path::Path, bridge: &str, database_existed: bool) -> io::Result<()> {
    let path = dir.join("charging.identity");
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            let recorded = files::identity(&path).map_err(io::Error::other)?;
            if recorded != bridge.as_bytes() {
                return Err(io::Error::other("charging state identity mismatch"));
            }
            return Ok(());
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    if database_existed {
        return Err(io::Error::other("charging state identity mismatch"));
    }
    // Atomic publication avoids a partial marker being mistaken for a foreign bridge.
    // A uniquely named unfinished temporary file after a crash is never trusted.
    let temporary = dir.join(format!("charging.identity.{}.tmp", uuid::Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(bridge.as_bytes())?;
    file.sync_all()?;
    fs::rename(&temporary, &path)?;
    File::open(dir)?.sync_all()?;
    Ok(())
}
