use std::{env, error::Error, fs, path::Path};

use schemars::{JsonSchema, schema_for};
use serde_json::Value;
use uob_contracts::{
    Command, CommandResult, DataPointDescriptor, DataPointValue, EventEnvelope,
    ResourceCapabilities, ResourceRef, RuntimeIdentity, StationSnapshot, TraceRecord,
};

fn publish<T: JsonSchema>(output: &Path, name: &str) -> Result<(), Box<dyn Error>> {
    let schema = schema_for!(T);
    let mut document = serde_json::to_value(schema)?;
    let object = document
        .as_object_mut()
        .ok_or("generated schema root must be an object")?;
    object.insert(
        "$id".to_owned(),
        Value::String(format!(
            "https://schemas.universal-ocpp-bridge.dev/contracts/v1.0/{name}.schema.json"
        )),
    );
    object.insert(
        "x-uob-contract-version".to_owned(),
        serde_json::json!({ "major": 1, "revision": 0 }),
    );
    fs::write(
        output.join(format!("{name}.schema.json")),
        format!("{}\n", serde_json::to_string_pretty(&document)?),
    )?;
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let output = env::args_os()
        .nth(1)
        .ok_or("usage: export_public_schemas <output-directory>")?;
    let output = Path::new(&output);
    fs::create_dir_all(output)?;

    publish::<StationSnapshot>(output, "station-snapshot")?;
    publish::<ResourceRef>(output, "resource-ref")?;
    publish::<ResourceCapabilities>(output, "resource-capabilities")?;
    publish::<RuntimeIdentity>(output, "runtime-identity")?;
    publish::<DataPointDescriptor>(output, "data-point-descriptor")?;
    publish::<DataPointValue>(output, "data-point-value")?;
    publish::<Command<Value>>(output, "command")?;
    publish::<CommandResult>(output, "command-result")?;
    publish::<EventEnvelope<Value>>(output, "event-envelope")?;
    publish::<TraceRecord>(output, "trace-record")?;
    Ok(())
}
