use super::*;
use uob_application::{AdmissionLimit, FlowDiagnostics, RuntimeResourceLimits};
use uob_contracts::{PayloadSchemaId, PrivilegedOcppOperation, ProtocolEdition, StationSnapshot};
use uob_protocol_adapter::{PendingCall, SessionSubmitError};

const PROFILE_CAP: usize = 256 * 1024;

fn encoded(call: &OutboundCall) -> String {
    serde_json::to_string(&(2, &call.message_id, call.action.as_str(), &call.payload)).unwrap()
}

fn profile_call(id: &str, encoded_bytes: usize) -> OutboundCall {
    let mut call = OutboundCall {
        message_id: id.to_owned(),
        action: ProtocolActionName::new("SetChargingProfile").unwrap(),
        payload: json!({
            "evseId": 0,
            "chargingProfile": {
                "id": 1,
                "stackLevel": 0,
                "chargingProfilePurpose": "ChargingStationMaxProfile",
                "chargingProfileKind": "Absolute",
                "chargingSchedule": [{
                    "id": 1,
                    "startSchedule": "2026-09-01T01:00:00Z",
                    "chargingRateUnit": "A",
                    "chargingSchedulePeriod": [{"startPeriod": 0, "limit": 10}]
                }]
            }
        }),
        correlation_id: CorrelationId::new("profile-frame-cap").unwrap(),
    };
    let limit = &mut call.payload["chargingProfile"]["chargingSchedule"][0]["chargingSchedulePeriod"]
        [0]["limit"];
    *limit = serde_json::from_str("1e1").unwrap();
    let padding = encoded_bytes.checked_sub(encoded(&call).len()).unwrap();
    // Leading exponent zeroes preserve the exact legal value 10 while retaining
    // a large numeric wire lexeme, without oversized IDs or unsupported fields.
    call.payload["chargingProfile"]["chargingSchedule"][0]["chargingSchedulePeriod"][0]["limit"] =
        serde_json::from_str(&format!("1e{}1", "0".repeat(padding))).unwrap();
    assert_eq!(encoded(&call).len(), encoded_bytes);

    let snapshot: StationSnapshot = serde_json::from_slice(include_bytes!(
        "../../../../crates/contracts/tests/fixtures/station-snapshot-ocpp201-v1.json"
    ))
    .unwrap();
    let mut station = snapshot.station;
    station.native_protocol_reference = None;
    assert_eq!(
        uob_protocol_adapter::command_registry::validate_privileged_operation(
            &station,
            &PrivilegedOcppOperation {
                protocol: ProtocolEdition::Ocpp201,
                action: call.action.clone(),
                payload_schema: PayloadSchemaId::new(
                    "urn:OCPP:Cp:2:2020:3:SetChargingProfileRequest",
                )
                .unwrap(),
                payload: call.payload.clone(),
            },
        ),
        Ok(()),
        "the frame-size fixture must be a valid native201 profile",
    );
    call
}

async fn acknowledge(running: &mut RunningSession, pending: PendingCall, id: &str) {
    running
        .peer
        .send_text(json!([3, id, {"status": "Accepted"}]).to_string())
        .await
        .unwrap();
    assert!(matches!(
        timeout(TEST_BOUND, pending.receive()).await.unwrap(),
        SessionCallOutcome::Result { payload, .. } if payload["status"] == "Accepted"
    ));
}

async fn barrier(running: &mut RunningSession) {
    let pending = running
        .handle
        .try_call(outbound("after-rejection", "cap-barrier"))
        .unwrap();
    let frame = receive_json(&mut running.peer).await;
    assert_eq!(frame[1], "after-rejection");
    assert_eq!(frame[2], "Reset");
    acknowledge(running, pending, "after-rejection").await;
}

#[tokio::test]
async fn native201_profile_hard_cap_counts_encoded_envelope_under_larger_global_budget() {
    let mut running = session_with_limits(
        "ocpp2.0.1",
        Duration::from_secs(2),
        FlowDiagnostics::default(),
        RuntimeResourceLimits {
            maximum_ocpp_message_bytes: 512 * 1024,
            ..RuntimeResourceLimits::default()
        },
    )
    .await;

    let at_limit = profile_call("at-limit\"\\id", PROFILE_CAP);
    let pending = running.handle.try_call(at_limit.clone()).unwrap();
    let frame = receive_json(&mut running.peer).await;
    assert_eq!(
        frame,
        json!([
            2,
            at_limit.message_id,
            "SetChargingProfile",
            at_limit.payload
        ])
    );
    acknowledge(&mut running, pending, &at_limit.message_id).await;

    let oversized = profile_call("over-limit\"\\id", PROFILE_CAP + 1);
    assert!(oversized.payload.to_string().len() < PROFILE_CAP);
    let escaping_bytes = serde_json::to_string(&oversized.message_id).unwrap().len()
        - (oversized.message_id.len() + 2);
    assert!(escaping_bytes > 0);
    assert!(encoded(&oversized).len() - escaping_bytes <= PROFILE_CAP);
    let admission = running.handle.try_call(oversized.clone());
    match admission {
        Err(SessionSubmitError::InvalidRequest) => {
            // A subsequent successful CALL is a socket-ordering barrier: the
            // rejected profile must not appear before it on the actual peer.
            barrier(&mut running).await;
            running.task.shutdown(Duration::from_secs(1)).await.unwrap();
            running.server.abort();
        }
        Ok(pending) => {
            let observed = receive_json(&mut running.peer).await;
            assert_eq!(observed[1], oversized.message_id);
            assert_eq!(observed[2], "SetChargingProfile");
            let observed_bytes = observed.to_string().len();
            acknowledge(&mut running, pending, &oversized.message_id).await;
            running.task.shutdown(Duration::from_secs(1)).await.unwrap();
            running.server.abort();
            panic!(
                "native201 profile hard cap failed: actual peer received {observed_bytes}-byte CALL"
            );
        }
        Err(error) => panic!("expected the profile hard cap, got {error:?}"),
    }
}

#[tokio::test]
async fn native201_profile_preserves_tighter_configured_message_budget() {
    let maximum = 1024;
    let mut running = session_with_limits(
        "ocpp2.0.1",
        Duration::from_secs(2),
        FlowDiagnostics::default(),
        RuntimeResourceLimits {
            maximum_ocpp_message_bytes: maximum,
            ..RuntimeResourceLimits::default()
        },
    )
    .await;

    let at_limit = profile_call("configured-limit", maximum);
    let pending = running.handle.try_call(at_limit.clone()).unwrap();
    let frame = receive_json(&mut running.peer).await;
    assert_eq!(frame[1], at_limit.message_id);
    assert_eq!(frame[2], "SetChargingProfile");
    acknowledge(&mut running, pending, &at_limit.message_id).await;

    assert!(matches!(
        running
            .handle
            .try_call(profile_call("configured-over", maximum + 1)),
        Err(SessionSubmitError::Resource(error))
            if error.limit == AdmissionLimit::OcppMessageBytes
                && error.maximum == maximum
                && error.requested == maximum + 1
    ));
    barrier(&mut running).await;
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
}
