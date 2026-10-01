use serde::Serialize;
use std::io;
use uob_application::{StorageError, StorageErrorCode};
use uob_contracts::{
    CommandResult, DEVICE_MODEL_OUTPUT_LIMIT_201, DeviceReportFailure201, DeviceReportState201,
};

struct Counter {
    bytes: usize,
    maximum: usize,
}
impl io::Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .filter(|size| *size <= self.maximum)
            .ok_or_else(|| io::Error::other("bounded device-model serialization"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub(super) fn fits(value: &impl Serialize) -> bool {
    serde_json::to_writer(
        Counter {
            bytes: 0,
            maximum: DEVICE_MODEL_OUTPUT_LIMIT_201,
        },
        value,
    )
    .is_ok()
}
pub(crate) fn bound_output(result: &mut CommandResult) -> Result<(), StorageError> {
    if result.device_model_201.is_none() || fits(result) {
        return Ok(());
    }
    if let Some(evidence) = &mut result.device_model_201
        && let DeviceReportState201::Complete { progress, .. } = &evidence.report
    {
        evidence.report = DeviceReportState201::Incomplete {
            reason: DeviceReportFailure201::OutputLimit,
            progress: Some(*progress),
        };
    }
    if fits(result) {
        Ok(())
    } else {
        Err(StorageError::new(
            StorageErrorCode::InvalidRequest,
            "device-model result exceeds bounded output",
        ))
    }
}
