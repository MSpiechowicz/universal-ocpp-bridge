use serde_json::{Value, json};
use uob_contracts::CommandResult;

pub const WHEN: &str = "2026-09-01T12:00:00Z";

pub fn target_origin(principal: &str, target: &str) -> Value {
    json!({"kind":"target", "target_instance_id":target, "principal_id":principal})
}

pub fn result(id: &str, origin: Value, station: &str, evidence: Value) -> CommandResult {
    let mut value = json!({
        "schema_version":{"major":1,"revision":5},
        "correlation_id":"device-query-109",
        "resource":{"bridge_id":"site-01","station_id":station},
        "return_route":{"request_id":id},
        "lifecycle":{"stage":"protocol_response","accepted":true},
        "recorded_at":WHEN
    });
    value["return_route"]["origin"] = origin;
    value["device_model_201"] = evidence;
    serde_json::from_value(value).unwrap()
}

fn context(query: Value, report: Value) -> Value {
    let mut value = json!({
        "connection":"authenticated-connection-109",
        "generation":7,
        "dispatch_recorded_at":WHEN
    });
    value["query"] = query;
    value["report"] = report;
    value
}

pub fn variables() -> Value {
    let component = json!({"name":"DeviceDataCtrlr"});
    let variable = json!({"name":"ItemsPerMessage","instance":"GetVariables"});
    let mut evidence = context(
        json!({"action":"GetVariables", "entries":[{
            "component":component, "variable":variable
        }]}),
        json!({"state":"not_expected"}),
    );
    evidence["variables"] = json!([{
        "component":component,
        "variable":variable,
        "attribute_type":"Actual",
        "status":"Accepted",
        "value":{"present":true,"redacted":false,"empty":false,"value":"16"}
    }]);
    evidence
}

pub fn report(state: Value, ack: &str) -> Value {
    let mut evidence = context(
        json!({
            "action":"GetReport", "request_id":-109,
            "selectors":[{"component":{"name":"VendorCtrlr","instance":"primary"}}],
            "criteria":["Active","Enabled"]
        }),
        state,
    );
    evidence["native_ack"] = json!(ack);
    evidence
}

pub fn base_report(state: Value, ack: &str) -> Value {
    let mut evidence = context(
        json!({"action":"GetBaseReport", "request_id":109,"report_base":"FullInventory"}),
        state,
    );
    evidence["native_ack"] = json!(ack);
    evidence
}

pub fn complete_report(item_count: usize) -> Value {
    assert!((1..=1024).contains(&item_count));
    let items: Vec<_> = (0..item_count)
        .map(|index| {
            json!({
                "component":{"name":"VendorCtrlr","instance":"primary"},
                "variable":{"name":"PrivateSetting", "instance":format!("channel-{index}")},
                "attributes":[{
                    "attribute_type":"Actual",
                    "value":{"present":true,"redacted":true,"empty":false},
                    "mutability":"WriteOnly", "persistent":true, "constant":false
                }],
                "characteristics":{
                    "unit":"V", "data_type":"string", "supports_monitoring":false
                }
            })
        })
        .collect();
    let bytes = serde_json::to_vec(&items).unwrap().len();
    json!({
        "state":"complete",
        "progress":{"fragments":1,"items":item_count,"bytes":bytes},
        "fragments":[{"generated_at":WHEN,"sequence":0,"more":false,"items":item_count}],
        "items":items
    })
}

pub fn export_record(command_result: &Value) -> Value {
    json!({
        "metadata":{
            "identity":{"record_id":"device-query-record-109"},
            "schema_version":{"major":1,"revision":6},
            "runtime":{
                "environment":"demo", "release_id":"device-consumer-fixture",
                "release_digest":"sha256:109", "process_instance_id":"device-consumer-process"
            },
            "resource":command_result["resource"],
            "observed_at":WHEN, "sequence":1
        },
        "payload":{"kind":"command_result","data":command_result}
    })
}

pub fn export_batch(command_result: &Value) -> Value {
    json!({
        "batch_id":"device-batch-109",
        "destination":{"destination_id":"analytics","configuration_revision":1},
        "records":[export_record(command_result)]
    })
}
