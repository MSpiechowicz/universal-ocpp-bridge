use std::io::{self, Write};

use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PayloadEncodingError {
    TooLarge,
    Serialization,
}

pub(super) fn encode_json(
    value: &impl Serialize,
    maximum_bytes: usize,
) -> Result<String, PayloadEncodingError> {
    let mut writer = BoundedWriter {
        bytes: Vec::with_capacity(maximum_bytes.min(4 * 1024)),
        maximum_bytes,
        overflowed: false,
    };
    if serde_json::to_writer(&mut writer, value).is_err() {
        return Err(if writer.overflowed {
            PayloadEncodingError::TooLarge
        } else {
            PayloadEncodingError::Serialization
        });
    }
    String::from_utf8(writer.bytes).map_err(|_| PayloadEncodingError::Serialization)
}

struct BoundedWriter {
    bytes: Vec<u8>,
    maximum_bytes: usize,
    overflowed: bool,
}

impl Write for BoundedWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.len() > self.maximum_bytes.saturating_sub(self.bytes.len()) {
            self.overflowed = true;
            return Err(io::Error::other("serialized event exceeds its bound"));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{PayloadEncodingError, encode_json};

    #[test]
    fn serialization_stops_at_the_configured_bound() {
        assert_eq!(
            encode_json(&serde_json::json!({ "payload": "too large" }), 8),
            Err(PayloadEncodingError::TooLarge)
        );
        assert_eq!(
            encode_json(&serde_json::json!({ "ok": true }), 32).unwrap(),
            "{\"ok\":true}"
        );
    }
}
