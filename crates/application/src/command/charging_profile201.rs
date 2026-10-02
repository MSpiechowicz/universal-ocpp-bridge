use uob_contracts::{
    Command, CommandError, CommandErrorCode, CommandLifecycle, CommandResult, UtcTimestamp,
};

pub(super) fn rejected<P>(
    command: &Command<P>,
    code: CommandErrorCode,
    now: UtcTimestamp,
) -> CommandResult {
    rejected_detail(command, code, now, "charging profile admission rejected")
}

pub(super) fn rejected_detail<P>(
    command: &Command<P>,
    code: CommandErrorCode,
    now: UtcTimestamp,
    detail: &str,
) -> CommandResult {
    super::command_result(
        command,
        CommandLifecycle::Rejected {
            error: CommandError {
                code,
                detail: Some(detail.to_owned()),
            },
        },
        now,
    )
}
