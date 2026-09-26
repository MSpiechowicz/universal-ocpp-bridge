//! Opt-in demo station runtime; one private store and one bounded authenticated socket owner.
mod commands;
mod control_auth;
mod files;
mod provision;
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
use uob_application::{
    Application, AuthorizationGuardedCommandPort, CommandAdmissionPort, CommandCoordinator,
    LocalAuthorizationService, PageLimit, StationEvent,
};
use uob_contracts::{
    Environment, NativeProtocolReference, ProtocolEdition, ResourceRef, StationId,
    TargetInstanceId, TransactionSnapshot,
};
use uob_protocol_adapter::{
    OcppEndpoint, StationAuthenticator, StationCredential, StationRegistration,
};
use uob_storage_adapter::{DEFAULT_WORK_QUEUE_CAPACITY, SqliteOperationalStore};

use crate::configuration::charging::{ValidatedChargingConfiguration, ValidatedChargingStation};

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
    commands: Arc<commands::LiveCommands>,
    credentials: Option<Arc<control_auth::ControlCredentials>>,
    _directory: File,
    _lock: File,
}

pub(crate) struct ChargingRuntime {
    pub(crate) state: ChargingState,
    listener: TcpListener,
    endpoint: OcppEndpoint,
    receiver: uob_protocol_adapter::StationConnectionReceiver,
    resources: BTreeMap<StationId, Vec<ResourceRef>>,
    settings: BTreeMap<StationId, StationSettings>,
    target: Option<(TargetInstanceId, u64)>,
}

#[derive(Clone)]
pub(super) struct StationSettings {
    protocol: ProtocolEdition,
    start: Option<provision::StartIdentity>,
    change_availability: bool,
    allow_stop: bool,
    allow_charging_limit: bool,
}

impl StationSettings {
    fn apply_capabilities(&self, snapshot: &mut uob_contracts::StationSnapshot) {
        use uob_contracts::{Operation, ResourceCapabilities, SupportedOperation};
        let mut operations = Vec::new();
        if self.start.is_some() {
            operations.push(Operation::Start);
        }
        if self.allow_stop {
            operations.push(Operation::Stop);
        }
        if self.change_availability {
            operations.push(Operation::ProtocolAction {
                protocol: self.protocol,
                action: "ChangeAvailability".to_owned(),
            });
        }
        snapshot.capabilities = ResourceCapabilities {
            operations: operations
                .into_iter()
                .map(|operation| SupportedOperation {
                    operation,
                    parameters: vec![],
                })
                .collect(),
            ..ResourceCapabilities::default()
        };
        for entry in &mut snapshot.resources {
            entry.capabilities.operations.clear();
            if self.allow_charging_limit
                && matches!(
                    entry.resource.native_protocol_reference,
                    Some(
                        NativeProtocolReference::Ocpp16 { connector_id: 1.. }
                            | NativeProtocolReference::Ocpp201 { evse_id: 1.., .. }
                    )
                )
            {
                entry.capabilities.operations.push(SupportedOperation {
                    operation: Operation::SetChargingLimit,
                    parameters: vec![],
                });
            }
        }
    }
}

impl ChargingState {
    pub(crate) fn command_configuration(
        &self,
        application: &Application,
    ) -> io::Result<Option<uob_management_adapter::ManagementCommandConfiguration>> {
        let Some(credentials) = &self.credentials else {
            return Ok(None);
        };
        let clock: Arc<dyn uob_application::CommandClock> = Arc::new(runtime::Clock);
        let coordinator: Arc<dyn CommandAdmissionPort<Value>> = Arc::new(
            CommandCoordinator::new(Arc::new(self.store.clone()), self.commands.clone(), clock)
                .with_diagnostics(application.diagnostics().clone()),
        );
        let guard: Arc<dyn CommandAdmissionPort<Value>> =
            Arc::new(AuthorizationGuardedCommandPort::new(
                coordinator,
                self.authorization.clone(),
                Arc::new(|| uob_contracts::UtcTimestamp::new(time::OffsetDateTime::now_utc())),
            ));
        credentials
            .clone()
            .configuration(&self.roster, guard)
            .map(Some)
            .map_err(io::Error::other)
    }
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
        let (read_grant, control, privileged) = load_grants(&config, &mut seen)?;
        let StationRoster {
            registrations,
            resources,
            roster,
            mut settings,
            tokens,
        } = load_stations(config.stations, &mut seen)?;
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
        let mut start_refs = BTreeMap::new();
        for (station, identity) in provision::provision(&authorization, &tokens).await? {
            start_refs.insert(station.clone(), identity.reference.clone());
            settings
                .get_mut(&station)
                .ok_or_else(|| fail("unknown start station"))?
                .start = Some(identity);
        }
        let credentials = control
            .map(|control| {
                control_auth::ControlCredentials::new(
                    Environment::Demo,
                    &read_grant,
                    control,
                    privileged,
                    &roster,
                    start_refs,
                )
                .map(Arc::new)
            })
            .transpose()
            .map_err(fail)?;
        runtime::reconcile(&store, &resources, &settings, application.identity()).await?;
        let (endpoint, receiver) =
            OcppEndpoint::new(authenticator, application, capacity).map_err(io::Error::other)?;
        let listener = TcpListener::bind(config.listen_addr).await?;
        Ok(Self {
            state: ChargingState {
                store,
                roster,
                read_grant,
                authorization,
                commands: Arc::new(commands::LiveCommands::new()),
                credentials,
                _directory: directory,
                _lock: lock,
            },
            listener,
            endpoint,
            receiver,
            resources,
            settings,
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

struct StationRoster {
    registrations: Vec<(StationRegistration, ProtocolEdition, StationCredential)>,
    resources: BTreeMap<StationId, Vec<ResourceRef>>,
    roster: Vec<ResourceRef>,
    settings: BTreeMap<StationId, StationSettings>,
    tokens: Vec<(StationId, ProtocolEdition, ResourceRef, files::ReadGrant)>,
}

fn load_grants(
    config: &ValidatedChargingConfiguration,
    seen: &mut BTreeSet<(u64, u64)>,
) -> io::Result<(
    files::ReadGrant,
    Option<files::ReadGrant>,
    Option<files::ReadGrant>,
)> {
    let fail = io::Error::other;
    let grant_path = PathBuf::from(config.read_grant_file.as_str());
    let mut grant = files::secret(&grant_path, seen).map_err(fail)?;
    if !std::str::from_utf8(&grant).is_ok_and(|token| {
        uob_management_adapter::token_matches_environment(token, Environment::Demo)
    }) {
        grant.fill(0);
        return Err(fail("charging read grant audience invalid"));
    }
    let read_grant = files::grant(grant);
    let control = config
        .control_grant_file
        .as_ref()
        .map(|file| files::secret(&PathBuf::from(file.as_str()), seen).map(files::grant))
        .transpose()
        .map_err(fail)?;
    let privileged = config
        .privileged_grant_file
        .as_ref()
        .map(|file| files::secret(&PathBuf::from(file.as_str()), seen).map(files::grant))
        .transpose()
        .map_err(fail)?;
    Ok((read_grant, control, privileged))
}

fn load_stations(
    stations: Vec<ValidatedChargingStation>,
    seen: &mut BTreeSet<(u64, u64)>,
) -> io::Result<StationRoster> {
    let mut registrations = Vec::with_capacity(stations.len());
    let mut resources = BTreeMap::new();
    let mut roster = Vec::new();
    let mut settings = BTreeMap::new();
    let mut tokens = Vec::new();
    for station in stations {
        let path = PathBuf::from(station.credential_file.as_str());
        let mut secret = files::secret(&path, seen).map_err(io::Error::other)?;
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
        if let Some(file) = &station.start_token_file {
            let bytes =
                files::secret(&PathBuf::from(file.as_str()), seen).map_err(io::Error::other)?;
            tokens.push((
                station.station_id.clone(),
                station.protocol,
                station.resources[0].clone(),
                files::grant(bytes),
            ));
        }
        settings.insert(
            station.station_id.clone(),
            StationSettings {
                protocol: station.protocol,
                start: None,
                change_availability: station.change_availability,
                allow_stop: station.allow_stop,
                allow_charging_limit: station.allow_charging_limit,
            },
        );
        resources.insert(station.station_id, station.resources);
    }
    Ok(StationRoster {
        registrations,
        resources,
        roster,
        settings,
        tokens,
    })
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
