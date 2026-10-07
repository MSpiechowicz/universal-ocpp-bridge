use std::{env, error::Error, fs, path::Path};

use schemars::{JsonSchema, schema_for};
use serde_json::Value;
use uob_contracts::{
    Command, CommandResult, ConfigurationChangeReference, DataPointDescriptor, DataPointValue,
    EventEnvelope, ExportBatch, ExportRecord, ExportReport, GetDiagnosticsReference16,
    GetLogReference16, ReserveNowReference16, ReserveNowReference201, ResourceCapabilities,
    ResourceRef, RuntimeIdentity, SendLocalListReference16, SendLocalListReference201,
    ServiceIdentity, SetNetworkProfileReference201, SetVariablesReference201,
    SignedUpdateFirmwareReference16, StationSnapshot, TraceRecord, UpdateFirmwareReference16,
    UpdateFirmwareReference201,
};

fn publish<T: JsonSchema>(output: &Path, name: &str) -> Result<(), Box<dyn Error>> {
    let revision = match name {
        "export-record" | "export-batch" => 17,
        "command-result" => 16,
        "station-snapshot" | "configuration-change-reference" => 1,
        _ => 0,
    };
    let output = output.join(format!("v1.{revision}"));
    fs::create_dir_all(&output)?;
    let schema = schema_for!(T);
    let mut document = serde_json::to_value(schema)?;
    let object = document
        .as_object_mut()
        .ok_or("generated schema root must be an object")?;
    object.insert(
        "$id".to_owned(),
        Value::String(format!(
            "https://schemas.universal-ocpp-bridge.dev/contracts/v1.{revision}/{name}.schema.json"
        )),
    );
    object.insert(
        "x-uob-contract-version".to_owned(),
        serde_json::json!({ "major": 1, "revision": revision }),
    );
    let path = output.join(format!("{name}.schema.json"));
    let contents = format!("{}\n", serde_json::to_string_pretty(&document)?);
    if path.exists() {
        if fs::read_to_string(&path)? != contents {
            return Err(format!(
                "published schema differs from generated contract: {}",
                path.display()
            )
            .into());
        }
    } else {
        fs::write(path, contents)?;
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let output = env::args_os()
        .nth(1)
        .ok_or("usage: export_public_schemas <schemas-root>")?;
    let output = Path::new(&output);
    fs::create_dir_all(output)?;

    publish::<StationSnapshot>(output, "station-snapshot")?;
    publish::<ResourceRef>(output, "resource-ref")?;
    publish::<ResourceCapabilities>(output, "resource-capabilities")?;
    publish::<RuntimeIdentity>(output, "runtime-identity")?;
    publish::<ServiceIdentity>(output, "service-identity")?;
    publish::<DataPointDescriptor>(output, "data-point-descriptor")?;
    publish::<DataPointValue>(output, "data-point-value")?;
    publish::<Command<Value>>(output, "command")?;
    publish::<CommandResult>(output, "command-result")?;
    publish::<ConfigurationChangeReference>(output, "configuration-change-reference")?;
    publish::<SetVariablesReference201>(output, "set-variables-reference-201")?;
    publish::<SetNetworkProfileReference201>(output, "set-network-profile-reference-201")?;
    publish::<SendLocalListReference16>(output, "send-local-list-reference-16")?;
    publish::<SendLocalListReference201>(output, "send-local-list-reference-201")?;
    publish::<ReserveNowReference16>(output, "reserve-now-reference-16")?;
    publish::<ReserveNowReference201>(output, "reserve-now-reference-201")?;
    publish::<UpdateFirmwareReference16>(output, "update-firmware-reference-16")?;
    publish::<SignedUpdateFirmwareReference16>(output, "signed-update-firmware-reference-16")?;
    publish::<UpdateFirmwareReference201>(output, "update-firmware-reference-201")?;
    publish::<GetDiagnosticsReference16>(output, "get-diagnostics-reference-16")?;
    publish::<GetLogReference16>(output, "get-log-reference-16")?;
    publish::<EventEnvelope<Value>>(output, "event-envelope")?;
    publish::<TraceRecord>(output, "trace-record")?;
    publish::<ExportRecord>(output, "export-record")?;
    publish::<ExportBatch>(output, "export-batch")?;
    publish::<ExportReport>(output, "export-report")?;
    Ok(())
}
