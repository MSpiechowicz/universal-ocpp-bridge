use std::collections::{HashMap, VecDeque};

use uob_application::artifact_provider::{
    ArtifactProviderError, ArtifactSha256, UploadId, UploadRefusal, UploadStatus,
};

use crate::artifacts::TemporaryArtifact;

/// Bounded upload destinations, evicting the oldest idle one when full.
pub(super) struct UploadSlots {
    order: VecDeque<UploadId>,
    slots: HashMap<UploadId, Slot>,
    maximum: usize,
}

struct Slot {
    maximum_bytes: u64,
    state: SlotState,
}

enum SlotState {
    Pending,
    Receiving,
    Received {
        size_bytes: u64,
        sha256: ArtifactSha256,
        // Owns the unlinked spool file until the destination is evicted.
        _artifact: TemporaryArtifact,
    },
    Refused(UploadRefusal),
}

/// Why an upload attempt could not start.
pub(super) enum BeginError {
    Unknown,
    Conflict,
}

/// Outcome of one upload attempt.
pub(super) enum Completion {
    Received(TemporaryArtifact, ArtifactSha256),
    Refused(UploadRefusal),
}

impl UploadSlots {
    pub(super) fn new(maximum: usize) -> Self {
        Self {
            order: VecDeque::with_capacity(maximum),
            slots: HashMap::with_capacity(maximum),
            maximum,
        }
    }

    pub(super) fn open(
        &mut self,
        upload_id: UploadId,
        maximum_bytes: u64,
    ) -> Result<(), ArtifactProviderError> {
        if self.slots.len() >= self.maximum {
            let idle = self
                .order
                .iter()
                .position(|id| !matches!(self.slots[id].state, SlotState::Receiving))
                .ok_or(ArtifactProviderError::Capacity)?;
            if let Some(evicted) = self.order.remove(idle) {
                self.slots.remove(&evicted);
            }
        }
        self.order.push_back(upload_id.clone());
        self.slots.insert(
            upload_id,
            Slot {
                maximum_bytes,
                state: SlotState::Pending,
            },
        );
        Ok(())
    }

    pub(super) fn status(&self, upload_id: &UploadId) -> Option<UploadStatus> {
        Some(match &self.slots.get(upload_id)?.state {
            SlotState::Pending | SlotState::Receiving => UploadStatus::Pending,
            SlotState::Received {
                size_bytes, sha256, ..
            } => UploadStatus::Received {
                size_bytes: *size_bytes,
                sha256: *sha256,
            },
            SlotState::Refused(refusal) => UploadStatus::Refused(*refusal),
        })
    }

    /// Marks one attempt as receiving and returns the destination's byte cap.
    pub(super) fn begin(&mut self, upload_id: &UploadId) -> Result<u64, BeginError> {
        let slot = self.slots.get_mut(upload_id).ok_or(BeginError::Unknown)?;
        match slot.state {
            SlotState::Pending | SlotState::Refused(_) => {
                slot.state = SlotState::Receiving;
                Ok(slot.maximum_bytes)
            }
            SlotState::Receiving | SlotState::Received { .. } => Err(BeginError::Conflict),
        }
    }

    pub(super) fn finish(&mut self, upload_id: &UploadId, completion: Completion) {
        let Some(slot) = self.slots.get_mut(upload_id) else {
            return;
        };
        if !matches!(slot.state, SlotState::Receiving) {
            return;
        }
        slot.state = match completion {
            Completion::Received(artifact, sha256) => SlotState::Received {
                size_bytes: artifact.len(),
                sha256,
                _artifact: artifact,
            },
            Completion::Refused(refusal) => SlotState::Refused(refusal),
        };
    }

    /// Records a refusal for an attempt that never reached the receiving state.
    pub(super) fn refuse_idle(&mut self, upload_id: &UploadId, refusal: UploadRefusal) {
        if let Some(slot) = self.slots.get_mut(upload_id)
            && matches!(slot.state, SlotState::Pending | SlotState::Refused(_))
        {
            slot.state = SlotState::Refused(refusal);
        }
    }
}
