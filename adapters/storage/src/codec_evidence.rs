//! Dispatch of the per-workflow command and result invariants shared by every codec path.
use crate::codec::{configuration201, local_authorization16, local_authorization201};
use serde::Serialize;
use uob_application::StorageError;
use uob_contracts::{Command, CommandResult};

/// Every protected workflow's own invariants for the commands it owns.
pub(crate) fn validate_command_evidence<P: Serialize>(
    command: &Command<P>,
) -> Result<(), StorageError> {
    configuration201::validate_command(command)?;
    local_authorization16::validate_command(command)?;
    crate::reservation16::validation::validate_command(command)?;
    crate::reservation201::validation::validate_command(command)?;
    crate::firmware16::validation::validate_command(command)?;
    crate::firmware201::validation::validate_command(command)?;
    crate::diagnostics16::validation::validate_command(command)?;
    crate::diagnostics201::validation::validate_command(command)
}

/// Every protected workflow's own invariants for the evidence it owns in a command result.
pub(crate) fn validate_result_evidence(result: &CommandResult) -> Result<(), StorageError> {
    configuration201::validate_result(result)?;
    local_authorization16::validate_result(result)?;
    local_authorization201::validate_result(result)?;
    crate::reservation16::codec_validation::validate_result(result)?;
    crate::reservation201::codec_validation::validate_result(result)?;
    crate::firmware16::validation::validate_result(result)?;
    crate::firmware201::validation::validate_result(result)?;
    crate::diagnostics16::validation::validate_result(result)?;
    crate::diagnostics201::validation::validate_result(result)
}
