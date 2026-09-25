//! Protected configuration ingress and explicit, independently persisted later observations.
use super::{
    CommandAdmissionFuture, CommandCoordinator, CommandResult, integrity_error, map_storage_error,
};
use uob_contracts::{
    CONFIGURATION_CHANGE_REFERENCE_SCHEMA, ConfigurationChangeReference, ConfigurationObservation,
    ConfigurationResult, PrivilegedOcppOperation, RequestId,
};

pub(super) fn valid_protected_change<P: 'static>(operation: &PrivilegedOcppOperation<P>) -> bool {
    if operation.payload_schema.as_str() != CONFIGURATION_CHANGE_REFERENCE_SCHEMA {
        return false;
    }
    let Some(payload) =
        (&operation.payload as &dyn std::any::Any).downcast_ref::<serde_json::Value>()
    else {
        return false;
    };
    let Some(fields) = payload.as_object() else {
        return false;
    };
    if fields.len() != 2 {
        return false;
    }
    let (Some(key), Some(reference)) = (
        fields.get("key").and_then(serde_json::Value::as_str),
        fields
            .get("valueReference")
            .and_then(serde_json::Value::as_str),
    ) else {
        return false;
    };
    ConfigurationChangeReference::valid_parts(key, reference)
}

pub(super) fn valid_configuration_read<P: 'static>(operation: &PrivilegedOcppOperation<P>) -> bool {
    if operation.payload_schema.as_str() != "urn:OCPP:1.6:2019:12:GetConfigurationRequest" {
        return false;
    }
    let Some(payload) =
        (&operation.payload as &dyn std::any::Any).downcast_ref::<serde_json::Value>()
    else {
        return false;
    };
    let Some(fields) = payload.as_object() else {
        return false;
    };
    if fields.is_empty() {
        return true;
    }
    if fields.len() != 1 {
        return false;
    }
    fields
        .get("key")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|keys| {
            keys.len() <= 256
                && keys
                    .iter()
                    .all(|key| key.as_str().is_some_and(|key| key.chars().count() <= 50))
        })
}

impl<P, E, D, R> CommandCoordinator<P, E, D, R>
where
    P: Clone + Send + Sync + 'static,
    E: Send + 'static,
    D: Send + 'static,
    R: Send + 'static,
{
    /// Links a later explicit `GetConfiguration` read to a completed write, without claiming
    /// equality with a secret value or automatically replaying either command.
    #[must_use]
    pub fn reconcile_configuration_observation(
        &self,
        write_id: RequestId,
        read_id: RequestId,
    ) -> CommandAdmissionFuture<'_, Option<CommandResult>> {
        Box::pin(async move {
            let write = self
                .store
                .command_by_request_id(write_id.clone())
                .await
                .map_err(|error| map_storage_error(&error))?;
            let read = self
                .store
                .command_by_request_id(read_id.clone())
                .await
                .map_err(|error| map_storage_error(&error))?;
            let (Some(write), Some(read)) = (write, read) else {
                return Ok(None);
            };
            if write.resource != read.resource
                || write.origin != read.origin
                || write_id == read_id
                || read.admitted_at < write.admitted_at
            {
                return Err(integrity_error(
                    "configuration observation scope or order mismatch",
                ));
            }
            let Some(result) = self
                .store
                .command_result_by_request_id(write_id.clone())
                .await
                .map_err(|error| map_storage_error(&error))?
            else {
                return Ok(None);
            };
            let Some(read_result) = self
                .store
                .command_result_by_request_id(read_id.clone())
                .await
                .map_err(|error| map_storage_error(&error))?
            else {
                return Ok(None);
            };
            let Some(ConfigurationResult::Write { key, .. }) = result.configuration.as_ref() else {
                return Err(integrity_error(
                    "configuration observation requires a completed write",
                ));
            };
            let Some(ConfigurationResult::Read { keys, .. }) = read_result.configuration.as_ref()
            else {
                return Err(integrity_error(
                    "configuration observation requires a completed read",
                ));
            };
            if read_result.recorded_at < result.recorded_at {
                return Err(integrity_error(
                    "configuration observation preceded write response",
                ));
            }
            let Some(observed) = keys
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .find(|entry| entry.key == *key)
            else {
                return Err(integrity_error(
                    "configuration observation missing requested key",
                ));
            };
            self.store
                .append_configuration_observation(
                    write_id,
                    ConfigurationObservation {
                        read_request_id: read_id,
                        key: observed.clone(),
                    },
                )
                .await
                .map_err(|error| map_storage_error(&error))
        })
    }
}
