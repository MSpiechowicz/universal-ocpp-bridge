use std::{error::Error, fmt};

use uob_application::{PendingDelivery, TargetDelivery};
use uob_contracts::TargetInstanceId;

use crate::{
    BridgeTargetSelection, TargetRegistry, TargetSelectionError, ValidatedTargetSelection,
};

/// Immutable owner of target work across process restarts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetDestination {
    /// Stable configured target instance.
    pub target_instance_id: TargetInstanceId,
    /// Exact configuration revision used when work was admitted.
    pub configuration_revision: u64,
}

/// Pending critical work owned by one exact target destination.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetBacklogEntry {
    /// Instance and revision that must receive or dispose of this work.
    pub destination: TargetDestination,
    /// Number of critical deliveries awaiting terminal classification.
    pub pending_critical_deliveries: u64,
}

/// Bounded durable facts supplied by the host during configuration preview.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TargetBacklogState {
    /// Distinct destination owners represented by pending critical deliveries.
    pub entries: Vec<TargetBacklogEntry>,
}

/// Authorized terminal handling for old target work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetDispositionAction {
    /// Retain the payload in an operator-visible archive without dispatching it.
    Archive,
    /// Permanently discard the payload under destructive-operation authorization.
    Discard,
}

/// Durable audit proof authorizing one old destination's terminal disposition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditedTargetDisposition {
    /// Stable audit event recording authorization and actor context.
    pub audit_event_id: String,
    /// Exact old instance and revision covered by the audit event.
    pub destination: TargetDestination,
    /// Authorized archive or discard action.
    pub action: TargetDispositionAction,
}

/// Offline result exposed to configuration clients before a service restart.
pub struct TargetConfigurationPreview<E, P> {
    selection: ValidatedTargetSelection<E, P>,
    running_destination: Option<TargetDestination>,
    restart_required: bool,
    pending_critical_deliveries: u64,
    dispositions: Vec<AuditedTargetDisposition>,
}

impl<E: 'static, P: 'static> TargetConfigurationPreview<E, P> {
    /// Returns the destination that new work will use after restart.
    #[must_use]
    pub fn destination(&self) -> TargetDestination {
        self.selection.destination()
    }

    /// Returns the currently running destination supplied to the preview.
    #[must_use]
    pub const fn running_destination(&self) -> Option<&TargetDestination> {
        self.running_destination.as_ref()
    }

    /// Reports that activation requires stopping and restarting the service.
    #[must_use]
    pub const fn restart_required(&self) -> bool {
        self.restart_required
    }

    /// Returns the total critical backlog considered by this preview.
    #[must_use]
    pub const fn pending_critical_deliveries(&self) -> u64 {
        self.pending_critical_deliveries
    }

    /// Returns the audit proofs that must be applied before activation.
    #[must_use]
    pub fn dispositions(&self) -> &[AuditedTargetDisposition] {
        &self.dispositions
    }

    /// Consumes the preview into a startup selection after the caller completes restart handling.
    #[must_use]
    pub fn into_restart_selection(self) -> ValidatedTargetSelection<E, P> {
        self.selection
    }
}

impl<E: 'static, P: 'static> TargetRegistry<E, P> {
    /// Validates and previews a target change without constructing adapters or opening sockets.
    ///
    /// # Errors
    ///
    /// Rejects ordinary selection failures, malformed backlog facts, and destination changes with
    /// pending critical work that lack exact, non-empty audited archive/discard proofs.
    pub fn preview_change(
        &self,
        selection: BridgeTargetSelection,
        running_destination: Option<&TargetDestination>,
        backlog: &TargetBacklogState,
        dispositions: &[AuditedTargetDisposition],
    ) -> Result<TargetConfigurationPreview<E, P>, TargetChangeError> {
        let selection = self
            .validate(selection)
            .map_err(TargetChangeError::InvalidSelection)?;
        let destination = selection.destination();
        let pending = validate_backlog_transition(&destination, backlog, dispositions)?;
        Ok(TargetConfigurationPreview {
            restart_required: running_destination.is_some_and(|running| running != &destination),
            running_destination: running_destination.cloned(),
            selection,
            pending_critical_deliveries: pending,
            dispositions: dispositions.to_vec(),
        })
    }
}

impl<E: 'static, P: 'static> ValidatedTargetSelection<E, P> {
    /// Returns the exact instance and revision accepted for new work after startup.
    #[must_use]
    pub fn destination(&self) -> TargetDestination {
        TargetDestination {
            target_instance_id: self.target_id.clone(),
            configuration_revision: self.configuration().configuration().revision,
        }
    }

    /// Rejects runtime delivery work owned by another target instance or revision.
    ///
    /// # Errors
    ///
    /// Returns [`TargetChangeError::DeliveryDestinationMismatch`] instead of rerouting the payload.
    pub fn validate_delivery_destination<T>(
        &self,
        delivery: &TargetDelivery<T>,
    ) -> Result<(), TargetChangeError> {
        self.validate_destination(
            &delivery.target_instance_id,
            delivery.target_configuration_revision,
        )
    }

    /// Rejects recovered durable work owned by another target instance or revision.
    ///
    /// # Errors
    ///
    /// Returns [`TargetChangeError::DeliveryDestinationMismatch`] instead of rerouting the payload.
    pub fn validate_pending_delivery_destination<T>(
        &self,
        delivery: &PendingDelivery<T>,
    ) -> Result<(), TargetChangeError> {
        self.validate_destination(
            &delivery.target_instance_id,
            delivery.target_configuration_revision,
        )
    }

    fn validate_destination(
        &self,
        target_instance_id: &TargetInstanceId,
        configuration_revision: u64,
    ) -> Result<(), TargetChangeError> {
        let destination = self.destination();
        if &destination.target_instance_id == target_instance_id
            && destination.configuration_revision == configuration_revision
        {
            Ok(())
        } else {
            Err(TargetChangeError::DeliveryDestinationMismatch)
        }
    }
}

fn validate_backlog_transition(
    next: &TargetDestination,
    backlog: &TargetBacklogState,
    dispositions: &[AuditedTargetDisposition],
) -> Result<u64, TargetChangeError> {
    let pending = backlog.entries.iter().try_fold(0_u64, |total, entry| {
        total
            .checked_add(entry.pending_critical_deliveries)
            .ok_or(TargetChangeError::InvalidBacklogState)
    })?;
    for (index, entry) in backlog.entries.iter().enumerate() {
        if entry.pending_critical_deliveries != 0
            && backlog.entries[..index].iter().any(|previous| {
                previous.pending_critical_deliveries != 0
                    && previous.destination == entry.destination
            })
        {
            return Err(TargetChangeError::InvalidBacklogState);
        }
    }

    let changed = backlog
        .entries
        .iter()
        .filter(|entry| entry.pending_critical_deliveries != 0 && &entry.destination != next)
        .collect::<Vec<_>>();
    if changed.is_empty() {
        return if dispositions.is_empty() {
            Ok(pending)
        } else {
            Err(TargetChangeError::DispositionWithoutPendingChange)
        };
    }
    if dispositions.len() != changed.len() {
        return Err(TargetChangeError::PendingDestinationChange);
    }
    for entry in changed {
        let matching = dispositions
            .iter()
            .filter(|proof| proof.destination == entry.destination)
            .collect::<Vec<_>>();
        if matching.len() != 1 || matching[0].audit_event_id.trim().is_empty() {
            return Err(TargetChangeError::InvalidDispositionAudit);
        }
    }
    Ok(pending)
}

/// Sanitized target change and immutable-routing failures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TargetChangeError {
    /// Ordinary offline target selection validation failed.
    InvalidSelection(TargetSelectionError),
    /// Backlog owners were duplicated or the total count overflowed.
    InvalidBacklogState,
    /// Old critical work must drain or receive exact audited disposition.
    PendingDestinationChange,
    /// A disposition had no audit ID, duplicated a proof, or named the wrong owner.
    InvalidDispositionAudit,
    /// A disposition was supplied although no pending owner changes destination.
    DispositionWithoutPendingChange,
    /// A recovered or runtime delivery belongs to another instance or revision.
    DeliveryDestinationMismatch,
}

impl fmt::Display for TargetChangeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "target configuration change {self:?}")
    }
}

impl Error for TargetChangeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidSelection(source) => Some(source),
            _ => None,
        }
    }
}
