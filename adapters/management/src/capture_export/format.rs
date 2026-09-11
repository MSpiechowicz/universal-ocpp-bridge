use std::io::{self, Write};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uob_application::capture::{CaptureLevel, CaptureStatus, TraceWindow};
use uob_contracts::{ContractVersion, ServiceIdentity, StationId, TargetInstanceId};

use super::stream::Limits;

pub(super) const MAX_METADATA_BYTES: usize = 16 * 1024;

#[derive(Serialize)]
struct Manifest<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    schema_version: &'static str,
    trace_schema_version: ContractVersion,
    build_version: &'static str,
    identity: &'a ServiceIdentity,
    capture_id: u64,
    filters: Filters<'a>,
    configuration: Configuration,
    window: Value,
    limits: Value,
    replay: &'static str,
    history_complete: bool,
}
#[derive(Serialize)]
struct Filters<'a> {
    station_id: &'a Option<StationId>,
    target_id: &'a Option<TargetInstanceId>,
}
#[derive(Serialize)]
struct Configuration {
    capture_level: &'static str,
    persistence: &'static str,
}

pub(super) fn manifest(
    identity: &ServiceIdentity,
    status: &CaptureStatus,
    window: TraceWindow,
    limits: Limits,
) -> Result<Vec<u8>, serde_json::Error> {
    let value = Manifest {
        kind: "manifest",
        schema_version: "1.0",
        trace_schema_version: ContractVersion::V1_INITIAL,
        build_version: env!("CARGO_PKG_VERSION"),
        identity,
        capture_id: status.id,
        filters: Filters {
            station_id: &status.filter.station,
            target_id: &status.filter.target,
        },
        // Closed allowlist: no raw configuration, endpoints, credential paths or secrets.
        configuration: Configuration {
            capture_level: if status.level == CaptureLevel::Metadata {
                "metadata"
            } else {
                "redacted_payload"
            },
            persistence: "memory_only",
        },
        window: window_value(window),
        limits: json!({
            "bytes": limits.bytes, "records": limits.records,
            "lifetime_ms": limits.lifetime.as_millis(), "idle_ms": limits.idle.as_millis()
        }),
        replay: "best_effort",
        history_complete: false,
    };
    let mut writer = BoundedWriter(Vec::new());
    serde_json::to_writer(&mut writer, &value)?;
    writer.0.push(b'\n');
    Ok(writer.0)
}

pub(super) fn window_value(window: TraceWindow) -> Value {
    json!({
        "first_sequence":window.first_sequence, "next_sequence":window.next_sequence,
        "retained_records":window.retained_records, "retained_bytes":window.retained_bytes,
        "evicted_records":window.evicted_records, "dropped_records":window.dropped_records,
        "shed_records":window.shed_records
    })
}

// Borrowed serialization stops before large or heavily escaped identity values allocate output.
struct BoundedWriter(Vec<u8>);
impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) >= MAX_METADATA_BYTES {
            return Err(io::Error::other("capture metadata capacity"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn truncated(bytes: &[u8]) -> bool {
    #[derive(Default, Deserialize)]
    struct Details {
        #[serde(default)]
        truncated: bool,
    }
    #[derive(Deserialize)]
    struct Record {
        #[serde(default)]
        redacted_details: Details,
    }
    // Ignore safe scalar values without constructing maps or copying payloads.
    serde_json::from_slice::<Record>(bytes).map_or(true, |r| r.redacted_details.truncated)
}
