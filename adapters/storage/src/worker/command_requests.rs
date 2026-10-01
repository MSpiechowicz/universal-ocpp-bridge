use rusqlite::Connection;
use serde::{Serialize, de::DeserializeOwned};

use super::{Request, append_configuration_observation, respond};
use crate::{command, command_history};

pub(super) fn handle<C, E, D>(
    connection: &mut Connection,
    drain: &mut crate::drain::Drain,
    request: Request<C, E, D>,
) where
    C: DeserializeOwned + Serialize,
    E: DeserializeOwned,
    D: DeserializeOwned,
{
    match request {
        Request::CommandCandidates(bridge, station, before, after, limit, reply) => {
            respond(
                reply,
                command::candidates(
                    connection,
                    &bridge,
                    &station,
                    before,
                    after.as_deref(),
                    limit,
                ),
            );
        }
        Request::JournalEvent(id, bridge, station, reply) => {
            respond(
                reply,
                command::journal_event(connection, &id, &bridge, &station),
            );
        }
        Request::CommandHistory(query, scope, reply) => {
            respond(reply, command_history::read(connection, &query, &scope));
        }
        Request::AppendConfigurationObservation(write_id, observation, reply) => {
            respond(
                reply,
                append_configuration_observation(connection, drain, &write_id, observation),
            );
        }
        Request::TriggerPending(station, class, now, reply) => {
            respond(
                reply,
                crate::trigger::pending(connection, &station, class, now),
            );
        }
        Request::TriggerPending201(station, class, now, reply) => {
            respond(
                reply,
                crate::trigger201::pending(connection, &station, class, now),
            );
        }
        Request::TriggerCandidates(after, limit, reply) => {
            respond(
                reply,
                crate::trigger::candidates(connection, after.as_deref(), limit),
            );
        }
        Request::ReconcileTrigger(request_id, now, reply) => {
            respond(
                reply,
                drain
                    .check_completion_write()
                    .and_then(|()| drain.changed())
                    .and_then(|()| crate::trigger::reconcile(connection, &request_id, now)),
            );
        }
        Request::InterruptDeviceReports(reply) => {
            respond(reply, crate::device_model201::interrupt(connection));
        }
        Request::DeviceReport(id, evidence, lifecycle, now, reply) => {
            respond(
                reply,
                drain
                    .check_completion_write()
                    .and_then(|()| drain.changed())
                    .and_then(|()| {
                        crate::device_model201::finish(connection, &id, evidence, lifecycle, now)
                    }),
            );
        }
        Request::PruneCommands(now, reply) => {
            respond(reply, command::prune::<C>(connection, now));
        }
        _ => unreachable!("only command requests are dispatched here"),
    }
}
