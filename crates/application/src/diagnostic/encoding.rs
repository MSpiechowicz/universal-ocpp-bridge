//! Bounded allocation and encoded-size accounting at the sole diagnostic serializer.

use std::{collections::BTreeMap, io, io::Write, sync::Arc};

use uob_contracts::{ContractVersion, RedactedTraceDetails, TraceRecord, TraceTarget};

use super::{
    DiagnosticAttribute, DiagnosticDisclosureAudit, DiagnosticObservation,
    DiagnosticSerializationError, DiagnosticSummary, DiagnosticTraceContext,
    MAX_DIAGNOSTIC_RECORD_BYTES, OMITTED_VENDOR_PAYLOAD, REDACTED, SafeDiagnosticField,
    SanitizedDiagnostic,
};

// The encoded-byte limit is supplemented by an entry limit, bounding map/audit overhead even
// for many tiny attributes. Duplicate safe names and sensitive classes share one audit entry.
const MAX_SAFE_FIELDS: usize = 64;
const EXPOSED_FIELDS: &str = "audit.exposed_fields";
const OMITTED_FIELDS: &str = "details.omitted_fields";

pub(super) fn serialize(
    context: DiagnosticTraceContext,
    observation: DiagnosticObservation,
) -> Result<SanitizedDiagnostic, DiagnosticSerializationError> {
    let (mut fields, mut audit, source_omitted) = required_fields(&observation);
    // Reserve the widest counter and the truncation flag before admitting any source values.
    fields.insert(OMITTED_FIELDS.to_owned(), usize::MAX.to_string());
    let mut record = record(context, observation.summary, fields);
    let mut counter = BoundedWriter::counting();
    serde_json::to_writer(&mut counter, &record).map_err(DiagnosticSerializationError)?;
    let mut encoded_size = counter.len;
    let mut omitted_fields = 0usize;
    let details = record
        .redacted_details
        .as_mut()
        .expect("diagnostic details");

    // Safe scalar values are borrowed until their escaped JSON representation fits the budget.
    for attribute in observation.attributes {
        if let DiagnosticAttribute::Safe(field) = attribute
            && !expose_field(&field, &mut details.fields, &mut audit, &mut encoded_size)
        {
            omitted_fields = omitted_fields.saturating_add(1);
        }
    }
    details
        .fields
        .insert(EXPOSED_FIELDS.to_owned(), audit.exposed_fields.join(","));
    details
        .fields
        .insert(OMITTED_FIELDS.to_owned(), omitted_fields.to_string());
    details.truncated =
        source_omitted || omitted_fields != 0 || audit.omitted_unknown_vendor_payloads != 0;
    let mut writer = BoundedWriter::buffered(encoded_size);
    serde_json::to_writer(&mut writer, &record).map_err(DiagnosticSerializationError)?;
    Ok(SanitizedDiagnostic {
        encoded_json: Arc::from(writer.bytes.expect("buffered writer")),
        audit: Arc::new(audit),
    })
}

fn expose_field(
    field: &SafeDiagnosticField,
    fields: &mut BTreeMap<String, String>,
    audit: &mut DiagnosticDisclosureAudit,
    encoded_size: &mut usize,
) -> bool {
    let (name, value) = field.render();
    let previous = fields.get(name.as_ref());
    let is_new = previous.is_none();
    if value.len() > MAX_DIAGNOSTIC_RECORD_BYTES
        || (is_new && audit.exposed_fields.len() >= MAX_SAFE_FIELDS)
    {
        return false;
    }
    let value_size = encoded_string_size(&value);
    let next_size = if let Some(previous) = previous {
        encoded_size
            .saturating_sub(encoded_string_size(previous))
            .saturating_add(value_size)
    } else {
        let name_size = encoded_string_size(&name);
        // One field entry (key, colon, value, comma) and its name in the audit's joined string.
        let audit_growth = name_size
            .saturating_sub(2)
            .saturating_add(usize::from(!audit.exposed_fields.is_empty()));
        encoded_size
            .saturating_add(name_size)
            .saturating_add(value_size)
            .saturating_add(2)
            .saturating_add(audit_growth)
    };
    if next_size > MAX_DIAGNOSTIC_RECORD_BYTES {
        return false;
    }
    fields.insert(name.to_string(), value.into_owned());
    if is_new {
        audit.exposed_fields.push(name.into_owned());
    }
    *encoded_size = next_size;
    true
}

fn required_fields(
    observation: &DiagnosticObservation,
) -> (BTreeMap<String, String>, DiagnosticDisclosureAudit, bool) {
    let mut fields = BTreeMap::new();
    let mut audit = DiagnosticDisclosureAudit::default();
    let mut original_size = 0usize;
    let mut vendor_original_size = 0usize;
    let mut source_omitted = false;
    for attribute in &observation.attributes {
        match attribute {
            DiagnosticAttribute::Safe(field) => {
                source_omitted |= matches!(field, SafeDiagnosticField::StateDetailsOmitted);
                // Unbounded identity strings remain borrowed; only fixed-size scalars render.
                original_size = original_size.saturating_add(field.render().1.len());
            }
            DiagnosticAttribute::Sensitive(value) => {
                let class = value.class.audit_name();
                if !audit.redacted_classes.contains(&class) {
                    fields.insert(format!("redacted.{class}"), REDACTED.to_owned());
                    audit.redacted_classes.push(class);
                }
            }
            DiagnosticAttribute::UnknownVendorPayload(value) => {
                vendor_original_size = vendor_original_size.saturating_add(value.original_size());
                audit.omitted_unknown_vendor_payloads =
                    audit.omitted_unknown_vendor_payloads.saturating_add(1);
            }
        }
    }
    if audit.omitted_unknown_vendor_payloads != 0 {
        fields.insert(
            "vendor_payload".to_owned(),
            OMITTED_VENDOR_PAYLOAD.to_owned(),
        );
        fields.insert(
            "vendor_payload.original_size".to_owned(),
            vendor_original_size.to_string(),
        );
    }
    fields.insert(
        "details.original_size".to_owned(),
        original_size
            .saturating_add(vendor_original_size)
            .to_string(),
    );
    fields.insert(EXPOSED_FIELDS.to_owned(), String::new());
    fields.insert(
        "audit.redacted_classes".to_owned(),
        audit.redacted_classes.join(","),
    );
    fields.insert(
        "audit.omitted_unknown_vendor_payloads".to_owned(),
        audit.omitted_unknown_vendor_payloads.to_string(),
    );
    (fields, audit, source_omitted)
}

fn record(
    context: DiagnosticTraceContext,
    summary: DiagnosticSummary,
    fields: BTreeMap<String, String>,
) -> TraceRecord {
    TraceRecord {
        schema_version: ContractVersion::V1_INITIAL,
        trace_id: context.trace_id,
        process_instance_id: context.process_instance_id,
        trace_sequence: context.trace_sequence,
        target: context
            .target
            .map(|(instance_id, kind)| TraceTarget { instance_id, kind }),
        correlation_id: context.correlation_id,
        parent_trace_id: context.parent_trace_id,
        stage: context.stage,
        direction: context.direction,
        observed_at: context.observed_at,
        duration_micros: context.duration_micros,
        outcome: context.outcome.into_contract(),
        redacted_details: Some(RedactedTraceDetails {
            summary: Some(summary.as_str().to_owned()),
            fields,
            truncated: true,
        }),
    }
}

// serde_json emits non-ASCII UTF-8 unchanged, escapes quotes/backslashes, uses two bytes for
// short control escapes, and six for the remaining ASCII controls. The quotes count as two.
fn encoded_string_size(value: &str) -> usize {
    let mut size = 2usize;
    for byte in value.bytes() {
        size += match byte {
            b'"' | b'\\' | b'\x08' | b'\x0c' | b'\n' | b'\r' | b'\t' => 2,
            0..=0x1f => 6,
            _ => 1,
        };
        if size > MAX_DIAGNOSTIC_RECORD_BYTES {
            return MAX_DIAGNOSTIC_RECORD_BYTES + 1;
        }
    }
    size
}

struct BoundedWriter {
    bytes: Option<Vec<u8>>,
    len: usize,
    limit: usize,
}

impl BoundedWriter {
    const fn counting() -> Self {
        Self {
            bytes: None,
            len: 0,
            limit: MAX_DIAGNOSTIC_RECORD_BYTES,
        }
    }

    fn buffered(limit: usize) -> Self {
        let limit = limit.min(MAX_DIAGNOSTIC_RECORD_BYTES);
        Self {
            bytes: Some(Vec::with_capacity(limit)),
            len: 0,
            limit,
        }
    }
}

impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.len) {
            return Err(io::Error::other(
                "sanitized diagnostic exceeds its byte limit",
            ));
        }
        if let Some(output) = &mut self.bytes {
            output.extend_from_slice(bytes);
        }
        self.len += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{BoundedWriter, MAX_DIAGNOSTIC_RECORD_BYTES, Write, encoded_string_size};

    #[test]
    fn escaped_size_accounting_matches_the_serializer() {
        let mut value: String = (0..=127).map(char::from).collect();
        value.push_str("λ😀é\u{2028}\u{2029}");
        assert_eq!(
            encoded_string_size(&value),
            serde_json::to_string(&value).unwrap().len()
        );
    }

    #[test]
    fn the_writer_rejects_overflow_before_growing_its_allocation() {
        let mut writer = BoundedWriter::buffered(16);
        writer.write_all(b"accepted").unwrap();
        assert!(writer.write_all(b"too many extra bytes").is_err());
        let bytes = writer.bytes.unwrap();
        assert_eq!(bytes, b"accepted");
        assert_eq!(bytes.capacity(), 16);

        let mut counter = BoundedWriter::counting();
        assert!(
            counter
                .write_all(&vec![0; MAX_DIAGNOSTIC_RECORD_BYTES + 1])
                .is_err()
        );
        assert_eq!(counter.len, 0);
        assert!(counter.bytes.is_none());
    }
}
