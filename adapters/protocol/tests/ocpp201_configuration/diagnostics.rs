use crate::{
    configuration_support::{configured, entry, learn, stop, variables},
    support::*,
};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use uob_application::{capture::*, *};
use uob_contracts::*;

#[tokio::test]
async fn accepted_and_mixed_native_writes_emit_value_free_correlated_protocol_response_traces() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let entries = vec![entry("First", 1), entry("Second", 2)];
    let (snapshot, _, port, _) = Box::pin(configured(
        &store,
        &running,
        entries
            .iter()
            .cloned()
            .map(|entry| (entry, "TRACE_NATIVE_SECRET".into()))
            .collect(),
        None,
        Arc::new(Clock),
    ))
    .await;
    let app = crate::endpoint_support::application(Environment::Demo, None);
    let capture = CaptureManager::new(true);
    let grant = CaptureGrant::new(
        app.identity().bridge_id.clone(),
        vec![CapturePermission::Capture],
        None,
        None,
    )
    .unwrap();
    capture
        .start(
            &grant,
            CaptureFilter {
                bridge: app.identity().bridge_id.clone(),
                station: None,
                target: None,
            },
            CaptureLevel::Metadata,
            None,
        )
        .unwrap();
    let (diagnostics, records) = FlowDiagnostics::channel(
        app.runtime_identity().process_instance_id.clone(),
        app.identity().bridge_id.clone(),
        capture,
        Arc::new(Clock),
        128,
    )
    .unwrap();
    let coordinator = Arc::new(
        Coordinator::new(Arc::new(store.clone()), port, Arc::new(Clock))
            .with_diagnostics(diagnostics),
    );
    let commands = Arc::new(scoped(
        coordinator,
        &snapshot,
        vec![
            AccessPermission::Control,
            AccessPermission::PrivilegedControl,
        ],
    ));
    learn(&mut running, &snapshot, &commands, 2, 4096, "trace").await;
    for (id, second_status, accepted) in [
        ("trace-accepted", "Accepted", true),
        ("trace-mixed", "Rejected", false),
    ] {
        let request = variables(&snapshot, id, &entries);
        let submitted = {
            let commands = commands.clone();
            tokio::spawn(async move { commands.submit(request).await.unwrap() })
        };
        assert_eq!(
            receive_json(&mut running.peer).await,
            json!([2,id,"SetVariables",{"setVariableData":[
                {"component":{"name":"VendorCtrlr"},"variable":{"name":"First"},"attributeValue":"TRACE_NATIVE_SECRET"},
                {"component":{"name":"VendorCtrlr"},"variable":{"name":"Second"},"attributeValue":"TRACE_NATIVE_SECRET"}
            ]}])
        );
        running.peer.send_text(json!([3,id,{"setVariableResult":[
            {"component":{"name":"VendorCtrlr"},"variable":{"name":"First"},"attributeStatus":"Accepted"},
            {"component":{"name":"VendorCtrlr"},"variable":{"name":"Second"},"attributeStatus":second_status}
        ]}]).to_string()).await.unwrap();
        assert!(
            matches!(submitted.await.unwrap().lifecycle, CommandLifecycle::ProtocolResponse { accepted: actual, .. } if actual==accepted)
        );
    }
    assert_protocol_response_records(records.try_iter().collect());
    stop(running, store).await;
}

fn assert_protocol_response_records(records: Vec<SanitizedDiagnostic>) {
    let traces = records
        .iter()
        .map(|record| serde_json::from_slice::<TraceRecord>(record.encoded_json()).unwrap())
        .collect::<Vec<_>>();
    for (id, evidence) in [
        ("trace-accepted", "charger_accepted"),
        ("trace-mixed", "rejected"),
    ] {
        let matching = traces
            .iter()
            .filter(|trace| {
                trace.stage.as_str() == "command.protocol_response"
                    && trace.redacted_details.as_ref().is_some_and(|details| {
                        details
                            .fields
                            .get("command.request_id")
                            .is_some_and(|request| request == id)
                    })
            })
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1);
        assert_eq!(
            matching[0]
                .redacted_details
                .as_ref()
                .unwrap()
                .fields
                .get("evidence")
                .map(String::as_str),
            Some(evidence)
        );
    }
    for record in records {
        let encoded = String::from_utf8_lossy(record.encoded_json());
        for forbidden in [
            "TRACE_NATIVE_SECRET",
            "cfg201:",
            "attributeValue",
            "valueReference",
        ] {
            assert!(!encoded.contains(forbidden));
        }
    }
}
