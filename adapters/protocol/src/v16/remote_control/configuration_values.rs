//! Locally provisioned protected configuration values; no raw values enter durable commands.
use serde::Serialize;
use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};
use uob_application::{CommandClock, StationCommandError};
use uob_contracts::{ConfigurationChangeReference, ResourceRef, UtcTimestamp};

/// OCPP 1.6 configuration text including the schema-permitted empty string.
/// Unlike authorization tokens, an empty configuration value is meaningful.
/// Never Debug/Serialize this type; its storage is overwritten when released.
pub struct ProtectedConfigurationText(Vec<u8>);

impl ProtectedConfigurationText {
    /// Takes ownership of trusted UTF-8 value material without storing it in any command.
    /// # Errors
    /// Rejects values exceeding the OCPP configuration text limit of 500 characters.
    pub fn new(value: String) -> Result<Self, StationCommandError> {
        if value.chars().count() > 500 {
            return Err(StationCommandError::new(
                "invalid protected configuration value",
            ));
        }
        Ok(Self(value.into_bytes()))
    }

    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("constructed from UTF-8")
    }
}

impl Drop for ProtectedConfigurationText {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

/// Trusted, externally provisioned value bound to exactly one station/key/reference.
/// The reference must be a separately generated 256-bit random capability (not a value digest).
pub struct ProtectedConfigurationValue {
    pub resource: ResourceRef,
    pub key: String,
    pub reference: String,
    pub value: ProtectedConfigurationText,
    pub expires_at: UtcTimestamp,
}

/// Opaque queued write. The provider is consulted again immediately before the socket send;
/// the queued request contains only the station/key/reference, never the protected value.
pub(crate) struct DeferredConfigurationCall {
    provider: Arc<LocalConfigurationValues>,
    clock: Arc<dyn CommandClock>,
    resource: ResourceRef,
    key: String,
    reference: String,
}

#[derive(Serialize)]
struct ConfigurationWire<'a> {
    key: &'a str,
    value: &'a str,
}

#[derive(Default)]
struct ByteCounter(usize);

impl std::io::Write for ByteCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl DeferredConfigurationCall {
    pub(crate) fn new(
        provider: Arc<LocalConfigurationValues>,
        clock: Arc<dyn CommandClock>,
        resource: ResourceRef,
        key: String,
        reference: String,
    ) -> Self {
        Self {
            provider,
            clock,
            resource,
            key,
            reference,
        }
    }

    pub(crate) fn wire_size(&self, message_id: &str) -> Option<usize> {
        self.provider.with_value(
            &self.reference,
            &self.resource,
            &self.key,
            self.clock.now(),
            |value| {
                let mut counter = ByteCounter::default();
                serde_json::to_writer(
                    &mut counter,
                    &(
                        2,
                        message_id,
                        "ChangeConfiguration",
                        ConfigurationWire {
                            key: &self.key,
                            value,
                        },
                    ),
                )
                .expect("bounded configuration frame is serializable");
                counter.0
            },
        )
    }

    pub(crate) fn encode_at_send(&self, message_id: &str) -> Option<String> {
        self.provider.with_value(
            &self.reference,
            &self.resource,
            &self.key,
            self.clock.now(),
            |value| {
                serde_json::to_string(&(
                    2,
                    message_id,
                    "ChangeConfiguration",
                    ConfigurationWire {
                        key: &self.key,
                        value,
                    },
                ))
                .expect("bounded configuration frame is serializable")
            },
        )
    }
}

/// Synchronous bounded memory-only resolver installed by the host on the live socket.
/// Dropping or revoking an entry destroys its zeroizing secret. This does not persist secrets.
pub struct LocalConfigurationValues {
    entries: RwLock<BTreeMap<String, ProtectedConfigurationValue>>,
}

impl LocalConfigurationValues {
    /// Validates the trusted provisioning set; never logs rejected material.
    /// # Errors
    /// Rejects oversized, duplicated or invalid station/key/reference provisioning.
    pub fn new(values: Vec<ProtectedConfigurationValue>) -> Result<Self, StationCommandError> {
        let invalid = || StationCommandError::new("invalid protected configuration provisioning");
        if values.len() > 128 {
            return Err(invalid());
        }
        let mut entries = BTreeMap::new();
        for entry in values {
            let value = entry.value.as_str();
            if entry.resource.resource.is_some()
                || !ConfigurationChangeReference::valid_parts(&entry.key, &entry.reference)
                || value.chars().count() > 500
                || entries.contains_key(&entry.reference)
            {
                return Err(invalid());
            }
            entries.insert(entry.reference.clone(), entry);
        }
        Ok(Self {
            entries: RwLock::new(entries),
        })
    }

    /// Removes a reference immediately; all subsequent dispatch attempts fail closed.
    /// # Errors
    /// Returns an error when the provider's lock is poisoned.
    pub fn revoke(&self, reference: &str) -> Result<(), StationCommandError> {
        self.entries
            .write()
            .map_err(|_| StationCommandError::new("configuration provider unavailable"))?
            .remove(reference);
        Ok(())
    }

    pub(super) fn authorized(
        &self,
        reference: &str,
        resource: &ResourceRef,
        key: &str,
        now: UtcTimestamp,
    ) -> bool {
        self.with_value(reference, resource, key, now, |_| ())
            .is_some()
    }

    fn with_value<T>(
        &self,
        reference: &str,
        resource: &ResourceRef,
        key: &str,
        now: UtcTimestamp,
        use_value: impl FnOnce(&str) -> T,
    ) -> Option<T> {
        let entries = self.entries.read().ok()?;
        let entry = entries.get(reference)?;
        if entry.resource != *resource || entry.key != key || now >= entry.expires_at {
            return None;
        }
        Some(use_value(entry.value.as_str()))
    }
}
