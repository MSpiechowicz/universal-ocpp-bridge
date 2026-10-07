//! Resolve crash-left dispatch uncertainty without scheduling recovered commands.
use std::{io, sync::Arc};

use uob_application::{CommandCoordinator, PageLimit};

use super::{ChargingStore, commands::LiveCommands, runtime::Clock};

pub(super) async fn recover(store: &ChargingStore, commands: Arc<LiveCommands>) -> io::Result<()> {
    uob_application::ReservationStore16::recover_reservations_16(
        store,
        uob_application::CommandClock::now(&Clock),
    )
    .await
    .map_err(io::Error::other)?;
    uob_application::ReservationStore201::recover_reservations_201(
        store,
        uob_application::CommandClock::now(&Clock),
    )
    .await
    .map_err(io::Error::other)?;
    uob_application::FirmwareStore16::recover_firmware_jobs_16(
        store,
        uob_application::CommandClock::now(&Clock),
    )
    .await
    .map_err(io::Error::other)?;
    uob_application::DiagnosticsStore16::recover_diagnostics_jobs_16(
        store,
        uob_application::CommandClock::now(&Clock),
    )
    .await
    .map_err(io::Error::other)?;
    uob_application::DiagnosticsStore201::recover_diagnostics_jobs_201(
        store,
        uob_application::CommandClock::now(&Clock),
    )
    .await
    .map_err(io::Error::other)?;
    uob_application::FirmwareStore201::recover_firmware_jobs_201(
        store,
        uob_application::CommandClock::now(&Clock),
    )
    .await
    .map_err(io::Error::other)?;
    let coordinator = CommandCoordinator::new(Arc::new(store.clone()), commands, Arc::new(Clock));
    let limit = PageLimit::new(100).map_err(io::Error::other)?;
    let mut after = None;

    loop {
        let batch = coordinator
            .recover_unresolved(after.clone(), limit)
            .await
            .map_err(io::Error::other)?;
        let Some(last) = batch.commands.last() else {
            return Ok(());
        };
        let next = last.command.request_id.clone();
        if after
            .as_ref()
            .is_some_and(|previous| next.as_str() <= previous.as_str())
        {
            return Err(io::Error::other("charging command recovery unavailable"));
        }
        // Admitted and already uncertain rows remain unresolved by design. Advance
        // by identity, not by repeatedly draining the unchanged first page.
        after = Some(next);
    }
}
