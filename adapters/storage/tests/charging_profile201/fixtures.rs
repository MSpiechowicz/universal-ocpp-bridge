use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use uob_application::*;
use uob_contracts::*;
use uob_storage_adapter::SqliteOperationalStore;
pub type Store = SqliteOperationalStore<Value, String, String, String>;
pub struct Database(pub PathBuf);
impl Database {
    pub fn new() -> Self {
        Self(std::env::temp_dir().join(format!("uob-profile201-{}.db", uuid::Uuid::new_v4())))
    }
    pub fn open(&self) -> Store {
        Store::open(&self.0, 32).unwrap()
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}
pub fn station() -> ResourceRef {
    ResourceRef {
        bridge_id: BridgeId::new("bridge").unwrap(),
        station_id: StationId::new("station").unwrap(),
        resource: None,
        native_protocol_reference: None,
    }
}
pub fn at(value: &str) -> UtcTimestamp {
    serde_json::from_value(json!(value)).unwrap()
}
pub fn footprint(
    id: i32,
    evse: i32,
    purpose: ChargingProfilePurpose201,
    stack: i32,
) -> ProfileFootprint201 {
    ProfileFootprint201 {
        id,
        evse_id: evse,
        purpose,
        stack_level: stack,
        transaction_id: None,
        valid_from: None,
        valid_to: None,
    }
}
pub fn clear(purpose: ChargingProfilePurpose201) -> ProfileMutation201 {
    ProfileMutation201::Clear(ClearChargingProfileRequest201 {
        charging_profile_id: None,
        charging_profile_criteria: Some(ChargingProfileCriteria201 {
            evse_id: None,
            stack_level: None,
            charging_profile_purpose: Some(purpose),
        }),
    })
}
pub fn set(footprint: ProfileFootprint201) -> ProfileMutation201 {
    ProfileMutation201::Set {
        footprint,
        full_native: false,
    }
}
pub async fn reserve(
    store: &Store,
    id: &str,
    mutation: ProfileMutation201,
    baseline: bool,
) -> Result<Command<Value>, StorageError> {
    let mut command: Command<Value> = serde_json::from_slice(include_bytes!(
        "../../../../crates/contracts/tests/fixtures/command-start-v1.json"
    ))
    .unwrap();
    command.request_id = RequestId::new(id).unwrap();
    command.resource = station();
    if let ProfileMutation201::Set { footprint, .. } = &mutation
        && footprint.evse_id > 0
    {
        command.resource.resource = Some(CanonicalResource::Evse {
            evse_id: CanonicalEvseId::new(format!("evse-{}", footprint.evse_id)).unwrap(),
            connector_id: None,
        });
        command.resource.native_protocol_reference = Some(NativeProtocolReference::Ocpp201 {
            evse_id: footprint.evse_id.cast_unsigned(),
            connector_id: None,
        });
    }
    command.operation = CommandOperation::SetChargingLimit(ChargingLimit {
        value: ExactDecimal::new(1, 0),
        unit: EngineeringUnit::Ampere,
        phases: None,
    });
    if let ProfileMutation201::Set {
        footprint,
        full_native: true,
    } = &mutation
    {
        let mut payload = json!({"evseId":footprint.evse_id,"chargingProfile":{
            "id":footprint.id,"stackLevel":footprint.stack_level,"chargingProfilePurpose":footprint.purpose,
            "chargingProfileKind":"Absolute","chargingSchedule":[{
                "id":footprint.id,"startSchedule":command.admitted_at,"chargingRateUnit":"A",
                "chargingSchedulePeriod":[{"startPeriod":0,"limit":1}]}]}});
        for (name, value) in [
            ("transactionId", json!(footprint.transaction_id)),
            ("validFrom", json!(footprint.valid_from)),
            ("validTo", json!(footprint.valid_to)),
        ] {
            if !value.is_null() {
                payload["chargingProfile"][name] = value;
            }
        }
        command.operation = CommandOperation::Ocpp(PrivilegedOcppOperation {
            protocol: ProtocolEdition::Ocpp201,
            action: ProtocolActionName::new("SetChargingProfile").unwrap(),
            payload_schema: PayloadSchemaId::new("urn:OCPP:Cp:2:2020:3:SetChargingProfileRequest")
                .unwrap(),
            payload,
        });
    }
    let reservation = ProfileReservation201 {
        station: station(),
        request_id: command.request_id.clone(),
        connection: CorrelationId::new("generation-1").unwrap(),
        generation: 1,
        requires_baseline: baseline,
        mutation,
    };
    let mut write = AtomicStoreWrite::empty();
    write.command = Some(command.clone());
    write.command_result = Some(result(&command, CommandLifecycle::Admitted));
    write.charging_profile_201 = Some(Box::new(reservation));
    store.write_atomic(write).await?;
    Ok(command)
}
pub fn result(command: &Command<Value>, lifecycle: CommandLifecycle) -> CommandResult {
    CommandResult {
        schema_version: ContractVersion::V1_INITIAL,
        resource: command.resource.clone(),
        correlation_id: command.correlation_id.clone(),
        return_route: command.return_route(),
        lifecycle,
        recorded_at: command.admitted_at,
        observed_effects: vec![],
        configuration: None,
        configuration_observations: vec![],
        trigger_observation: None,
        trigger_observation_201: None,
        composite_schedule_16: None,
        device_model_201: None,
        charging_profile_16: None,
        charging_profile_201: None,
        configuration_201: None,
        local_authorization_16: None,
        local_authorization_201: None,
        reservation_16: None,
        reservation_201: None,
    }
}
pub async fn persist(store: &Store, result: CommandResult) {
    let mut write = AtomicStoreWrite::empty();
    write.command_result = Some(result);
    store.write_atomic(write).await.unwrap();
}
pub async fn accepted(store: &Store, command: &Command<Value>) {
    persist(
        store,
        result(
            command,
            CommandLifecycle::ProtocolResponse {
                accepted: true,
                error: None,
            },
        ),
    )
    .await;
}
pub async fn clear_status(
    store: &Store,
    command: &Command<Value>,
    mutation: &ProfileMutation201,
    status: ClearChargingProfileStatus201,
) {
    let ProfileMutation201::Clear(request) = mutation else {
        panic!("clear")
    };
    let mut evidence = result(
        command,
        CommandLifecycle::ProtocolResponse {
            accepted: status == ClearChargingProfileStatus201::Accepted,
            error: None,
        },
    );
    evidence.schema_version = ContractVersion::V1_CHARGING_PROFILE_201;
    evidence.charging_profile_201 = Some(ChargingProfileResult201::ClearChargingProfile {
        request: request.clone(),
        status,
        reason_code: None,
    });
    persist(store, evidence).await;
}
pub async fn baseline(store: &Store) {
    for (index, purpose) in [
        ChargingProfilePurpose201::ChargingStationMaxProfile,
        ChargingProfilePurpose201::TxDefaultProfile,
        ChargingProfilePurpose201::TxProfile,
    ]
    .into_iter()
    .enumerate()
    {
        let mutation = clear(purpose);
        let command = reserve(store, &format!("baseline-{index}"), mutation.clone(), false)
            .await
            .unwrap();
        clear_status(
            store,
            &command,
            &mutation,
            ClearChargingProfileStatus201::Unknown,
        )
        .await;
    }
}
pub async fn close(store: &Store) {
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}
