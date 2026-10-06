use super::{
    CallContext, Clock, CommitState, call_error, complete_observation, context, effects, status,
    trigger, unavailable,
};
use std::{io, time::Duration};
use uob_contracts::{ProtocolEdition, StationSnapshot};
use uob_protocol_adapter::{IncomingCall, OcppErrorCode, v16, v201};
use uob_provider_adapter::{LocalAuthorizationProvider, LocalChargingIdentityProvider};
#[allow(clippy::too_many_lines)] // One routing table for every authenticated station call.
pub(super) async fn dispatch_call(
    incoming: IncomingCall,
    snapshot: &mut StationSnapshot,
    mut services: CallContext<'_>,
    protocol: ProtocolEdition,
) -> io::Result<()> {
    let mut commits = CommitState::default();
    let response = match (protocol, incoming.call.action.as_str()) {
        (_, "StatusNotification") => {
            status::complete_status(incoming.call, snapshot, &services, protocol, &mut commits)
                .await?
        }
        (ProtocolEdition::Ocpp16j, "Authorize") => {
            let parent = match &incoming.call.observation {
                uob_application::ChargerObservation::ChargingIdentity(identity) => services
                    .reservations
                    .and_then(|p| p.parent_for_wire(identity.token.as_bytes())),
                _ => None,
            };
            v16::complete_authorization(
                incoming.call,
                &snapshot.station,
                services.authorization,
                &LocalAuthorizationProvider,
                &Clock,
                Duration::from_secs(2),
            )
            .await
            .map(|mut response| {
                if response[2]["idTagInfo"]["status"] == "Accepted"
                    && let Some(parent) = parent
                    && let Ok(text) = std::str::from_utf8(parent.expose_to_provider())
                {
                    response[2]["idTagInfo"]["parentIdTag"] =
                        serde_json::Value::String(text.to_owned());
                }
                response
            })
        }
        (ProtocolEdition::Ocpp16j, "StartTransaction" | "StopTransaction") => {
            let context = context(
                services.store,
                services.identity,
                &incoming,
                services.target.take(),
            )
            .await?;
            commits.committed = Some(context.event_id.clone());
            let transaction_services = v16::TransactionServices {
                store: services.store,
                authorization: services.authorization,
                provider: &LocalAuthorizationProvider,
                clock: &Clock,
                authorization_timeout: Duration::from_secs(2),
                reservation_values: services.reservations,
            };
            v16::complete_transaction(incoming.call, snapshot, &transaction_services, context).await
        }
        (ProtocolEdition::Ocpp201, "Authorize") => {
            v201::complete_authorization(
                incoming.call,
                &snapshot.station,
                services.authorization,
                &LocalChargingIdentityProvider,
                &Clock,
                Duration::from_secs(2),
            )
            .await
        }
        (
            ProtocolEdition::Ocpp201,
            "NotifyEVChargingNeeds"
            | "NotifyEVChargingSchedule"
            | "NotifyChargingLimit"
            | "ClearedChargingLimit",
        ) => super::negotiation201::complete(&incoming, snapshot, &services).await,
        (ProtocolEdition::Ocpp201, "ReservationStatusUpdate") => {
            super::calls::reservation_status_update(&incoming, snapshot, &services).await
        }
        (ProtocolEdition::Ocpp201, "TransactionEvent") | (_, "MeterValues") => {
            complete_observation(&incoming, snapshot, &mut services, protocol, &mut commits).await
        }
        (
            ProtocolEdition::Ocpp16j,
            "DiagnosticsStatusNotification" | "FirmwareStatusNotification",
        ) => status::complete_trigger_status(&incoming, snapshot, &services, &mut commits).await?,
        (
            ProtocolEdition::Ocpp201,
            "LogStatusNotification"
            | "FirmwareStatusNotification"
            | "PublishFirmwareStatusNotification"
            | "SignCertificate",
        ) => {
            status::complete_trigger_receipt_201(&incoming, snapshot, &services, &mut commits)
                .await?
        }
        _ => Err(call_error(protocol, OcppErrorCode::NotImplemented)),
    };
    if let (Ok(_), Some(event_id)) = (&response, commits.committed) {
        effects::reconcile(
            services.store,
            services.commands,
            &snapshot.station,
            event_id,
        )
        .await?;
    }
    if commits.trigger_committed {
        trigger::sweep(services.store, services.commands, &mut None).await?;
    }
    let failed_storage = response
        .as_ref()
        .is_err_and(|error| error.code == OcppErrorCode::InternalError);
    match response {
        Ok(response) => incoming
            .responder
            .respond(&response[2])
            .map_err(|_| unavailable())?,
        Err(error) => incoming
            .responder
            .reject(error)
            .map_err(|_| unavailable())?,
    }
    if failed_storage {
        Err(unavailable())
    } else {
        Ok(())
    }
}
