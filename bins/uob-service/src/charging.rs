//! Opt-in demo station runtime; one private store and one bounded authenticated socket owner.
mod commands;
mod configuration201;
mod control_auth;
mod device_model;
mod files;
mod firmware;
mod local_authorization;
mod profiles;
mod provision;
mod recovery;
mod reservations;
mod roster;
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
    Application, AuthorizationGuardedCommandPort, ChargingProfileStore201, CommandAdmissionPort,
    CommandCoordinator, LocalAuthorizationService, PageLimit, StationEvent,
};
use uob_contracts::{
    Environment, NativeProtocolReference, ProtocolEdition, ResourceRef, StationId,
    TargetInstanceId, TransactionSnapshot,
};
use uob_protocol_adapter::{
    OcppEndpoint, StationAuthenticator,
    v201::remote_control::configuration201_values::LocalConfigurationValues201,
};
use uob_storage_adapter::{DEFAULT_WORK_QUEUE_CAPACITY, SqliteOperationalStore};

use crate::configuration::charging::{StationControlOptions, ValidatedChargingConfiguration};

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
    protected_configuration: Option<Arc<LocalConfigurationValues201>>,
    protected_local_authorization: local_authorization::Providers,
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
    artifact_server: Option<firmware::ArtifactServer>,
}

#[derive(Clone)]
pub(super) struct StationSettings {
    protocol: ProtocolEdition,
    start: Option<provision::StartIdentity>,
    control: StationControlOptions,
    configuration: Option<Arc<LocalConfigurationValues201>>,
    local_authorization:
        Option<Arc<uob_protocol_adapter::v16::remote_control::LocalAuthorizationUpdates16>>,
    local_authorization_201:
        Option<Arc<uob_protocol_adapter::v201::remote_control::LocalAuthorizationUpdates201>>,
    reservations: Option<Arc<uob_protocol_adapter::v16::remote_control::ReservationValues16>>,
    reservations_201: Option<Arc<uob_protocol_adapter::v201::remote_control::ReservationValues201>>,
    firmware: Option<crate::configuration::charging::StationFirmware>,
    firmware_providers: Option<Arc<firmware::Providers>>,
}

impl StationSettings {
    fn apply_capabilities(&self, snapshot: &mut uob_contracts::StationSnapshot) {
        use uob_contracts::{Operation, ResourceCapabilities, SupportedOperation};
        let mut operations = Vec::new();
        if self.start.is_some() {
            operations.push(Operation::Start);
        }
        if self.control.allow_stop {
            operations.push(Operation::Stop);
        }
        if self.control.change_availability {
            operations.push(Operation::ProtocolAction {
                protocol: self.protocol,
                action: "ChangeAvailability".to_owned(),
            });
        }
        if self.control.trigger_message.enabled() {
            operations.push(Operation::ProtocolAction {
                protocol: self.protocol,
                action: "TriggerMessage".to_owned(),
            });
        }
        if self.control.get_composite_schedule.enabled() {
            operations.push(Operation::ProtocolAction {
                protocol: self.protocol,
                action: "GetCompositeSchedule".to_owned(),
            });
        }
        self.add_local_authorization_operations(&mut operations);
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
            if self.control.trigger_message.enabled()
                && matches!(
                    entry.resource.native_protocol_reference,
                    Some(
                        NativeProtocolReference::Ocpp16 { connector_id: 1.. }
                            | NativeProtocolReference::Ocpp201 { evse_id: 1.., .. }
                    )
                )
            {
                entry.capabilities.operations.push(SupportedOperation {
                    operation: Operation::ProtocolAction {
                        protocol: self.protocol,
                        action: "TriggerMessage".to_owned(),
                    },
                    parameters: vec![],
                });
            }
            if self.control.get_composite_schedule.enabled()
                && self.protocol == ProtocolEdition::Ocpp16j
                && matches!(
                    entry.resource.native_protocol_reference,
                    Some(NativeProtocolReference::Ocpp16 { connector_id })
                        if connector_id > 0 && i32::try_from(connector_id).is_ok()
                )
            {
                entry.capabilities.operations.push(SupportedOperation {
                    operation: Operation::ProtocolAction {
                        protocol: self.protocol,
                        action: "GetCompositeSchedule".to_owned(),
                    },
                    parameters: vec![],
                });
            }
            if self.control.allow_charging_limit
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
        device_model::apply(snapshot, self.protocol, self.control);
        profiles::apply(snapshot, self.protocol, self.control);
        reservations::apply_capabilities(snapshot, self);
        firmware::apply_capabilities(
            snapshot,
            self.firmware.filter(|_| self.firmware_providers.is_some()),
        );
    }
    fn add_local_authorization_operations(&self, operations: &mut Vec<uob_contracts::Operation>) {
        for (enabled, action) in [
            (
                self.control.get_local_list_version.enabled(),
                "GetLocalListVersion",
            ),
            (self.control.send_local_list.enabled(), "SendLocalList"),
            (self.control.clear_cache.enabled(), "ClearCache"),
        ] {
            if enabled {
                operations.push(uob_contracts::Operation::ProtocolAction {
                    protocol: self.protocol,
                    action: action.to_owned(),
                });
            }
        }
    }
}

impl ChargingState {
    pub(crate) fn command_port(
        &self,
        application: &Application,
    ) -> Arc<dyn CommandAdmissionPort<Value>> {
        let clock: Arc<dyn uob_application::CommandClock> = Arc::new(runtime::Clock);
        let coordinator: Arc<dyn CommandAdmissionPort<Value>> = Arc::new(
            CommandCoordinator::new(Arc::new(self.store.clone()), self.commands.clone(), clock)
                .with_diagnostics(application.diagnostics().clone()),
        );
        let coordinator = Arc::new(profiles::supervisor::Supervisor(coordinator));
        Arc::new(AuthorizationGuardedCommandPort::new(
            coordinator,
            self.authorization.clone(),
            Arc::new(|| uob_contracts::UtcTimestamp::new(time::OffsetDateTime::now_utc())),
        ))
    }

    pub(crate) fn command_configuration(
        &self,
        application: &Application,
    ) -> io::Result<Option<uob_management_adapter::ManagementCommandConfiguration>> {
        let Some(credentials) = &self.credentials else {
            return Ok(None);
        };
        credentials
            .clone()
            .configuration(
                &self.roster,
                self.command_port(application),
                self.protected_configuration.clone(),
                self.protected_local_authorization.clone(),
            )
            .map(Some)
            .map_err(io::Error::other)
    }
}

impl ChargingRuntime {
    #[allow(clippy::too_many_lines)] // Startup checks and composition stay in one ordered sequence.
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
        let roster::StationRoster {
            registrations,
            resources,
            roster,
            mut settings,
            tokens,
        } = roster::load_stations(config.stations, &mut seen)?;
        let protected_configuration = configuration201::install(
            config.configuration_values_file.as_deref(),
            &resources,
            &mut settings,
            &mut seen,
        )?;
        let protected_local_authorization = local_authorization::install(
            config.local_authorization_updates_file.as_deref(),
            &resources,
            &mut settings,
            &mut seen,
        )?;
        let artifact_server =
            firmware::install(config.firmware, application, &mut seen, &mut settings).await?;
        let capacity = resources.len();
        let authenticator =
            StationAuthenticator::demo_with_protocols(registrations).map_err(io::Error::other)?;
        let store = open_store(
            &config.state_directory,
            application.identity().bridge_id.as_str(),
            &directory,
        )
        .await?;
        let commands = Arc::new(commands::LiveCommands::new());
        recovery::recover(&store, commands.clone()).await?;
        let authorization = Arc::new(
            ChargingAuthorization::recover(
                Arc::new(store.clone()),
                PageLimit::new(100).map_err(io::Error::other)?,
            )
            .await
            .map_err(io::Error::other)?,
        );
        reservations::provision_policy(&authorization, &settings, &resources).await?;
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
                    &resources,
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
                commands,
                credentials,
                protected_configuration,
                protected_local_authorization,
                _directory: directory,
                _lock: lock,
            },
            listener,
            endpoint,
            receiver,
            resources,
            settings,
            target,
            artifact_server,
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

async fn open_store(
    state_directory: &std::path::Path,
    bridge: &str,
    directory: &File,
) -> io::Result<ChargingStore> {
    let database = prepare_database(state_directory, bridge, directory)?;
    let store = SqliteOperationalStore::open(&database, DEFAULT_WORK_QUEUE_CAPACITY)
        .map_err(io::Error::other)?;
    files::check_database_files(state_directory).map_err(io::Error::other)?;
    store
        .interrupt_charging_profile_mutations()
        .await
        .map_err(io::Error::other)?;
    Ok(store)
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
