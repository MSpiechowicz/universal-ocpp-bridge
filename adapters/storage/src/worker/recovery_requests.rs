use rusqlite::Connection;
use serde::de::DeserializeOwned;

use super::{Request, respond};
use crate::recovery;

pub(super) fn handle<C, E, D>(connection: &Connection, request: Request<C, E, D>)
where
    C: DeserializeOwned,
    D: DeserializeOwned,
{
    match request {
        Request::Recover(query, reply) => respond(
            reply,
            recovery::recover(
                connection,
                query
                    .after_command
                    .as_ref()
                    .map(uob_contracts::RequestId::as_str),
                usize::from(query.limit.get()),
            ),
        ),
        Request::Command(request_id, reply) => {
            respond(reply, recovery::command(connection, &request_id));
        }
        Request::CommandResult(request_id, reply) => {
            respond(reply, recovery::command_result(connection, &request_id));
        }
        _ => unreachable!("only recovery reads are dispatched here"),
    }
}
