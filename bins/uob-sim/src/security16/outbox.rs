//! Durable security-event outbox: A04.FR.02 guaranteed delivery. An event stays queued across
//! disconnects and process restarts until the CSMS answers with `SecurityEventNotification.conf`.
use super::model::validate_event;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::VecDeque;

const OUTBOX_LIMIT: usize = 256;

/// One queued event; `duplicate` marks the fault-knob copy of an already acknowledged event.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SecurityEvent {
    pub id: u64,
    #[serde(rename = "type")]
    pub kind: String,
    pub timestamp: String,
    pub tech_info: Option<String>,
    pub duplicate: bool,
}

impl SecurityEvent {
    /// The exact native `SecurityEventNotification.req` payload.
    #[must_use]
    pub fn payload(&self) -> Value {
        let mut payload = json!({"type": self.kind, "timestamp": self.timestamp});
        if let Some(info) = &self.tech_info {
            payload["techInfo"] = info.clone().into();
        }
        payload
    }
}

#[derive(Clone, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct PrivateState {
    pub(super) format_version: u16,
    pub(super) station: String,
    pub(super) next_id: u64,
    pub(super) outbox: VecDeque<SecurityEvent>,
    pub(super) emitted: u64,
    pub(super) acknowledged: u64,
    pub(super) rejected: u64,
    pub(super) duplicates_sent: u64,
    pub(super) last_acknowledged_type: Option<String>,
    pub(super) revision: u64,
}

impl PrivateState {
    pub(super) fn empty(station: &str) -> Self {
        Self {
            format_version: 1,
            station: station.to_owned(),
            next_id: 0,
            outbox: VecDeque::new(),
            emitted: 0,
            acknowledged: 0,
            rejected: 0,
            duplicates_sent: 0,
            last_acknowledged_type: None,
            revision: 0,
        }
    }

    pub(super) fn validate(&self, station: &str) -> Result<(), &'static str> {
        let events_valid = self.outbox.iter().all(|event| {
            event.id != 0
                && event.id <= self.next_id
                && validate_event(&event.kind, &event.timestamp, event.tech_info.as_deref()).is_ok()
        });
        if self.format_version != 1
            || self.station != station
            || self.outbox.len() > OUTBOX_LIMIT
            || !events_valid
        {
            return Err("security_state_invalid");
        }
        Ok(())
    }

    fn next_id(&mut self) -> Result<u64, &'static str> {
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or("security_event_id_capacity")?;
        Ok(self.next_id)
    }

    /// Queue one event at the back; returns its identity.
    pub(super) fn queue(
        &mut self,
        kind: &str,
        timestamp: &str,
        tech_info: Option<&str>,
    ) -> Result<u64, &'static str> {
        validate_event(kind, timestamp, tech_info)?;
        if self.outbox.len() == OUTBOX_LIMIT {
            return Err("security_outbox_capacity");
        }
        let id = self.next_id()?;
        self.outbox.push_back(SecurityEvent {
            id,
            kind: kind.to_owned(),
            timestamp: timestamp.to_owned(),
            tech_info: tech_info.map(str::to_owned),
            duplicate: false,
        });
        self.emitted = self.emitted.saturating_add(1);
        Ok(id)
    }

    /// The CSMS confirmed the claimed front event. With the duplicate knob an identical copy is
    /// queued at the front so it follows its original immediately.
    pub(super) fn acknowledge(
        &mut self,
        event: &SecurityEvent,
        duplicate: bool,
    ) -> Result<(), &'static str> {
        if self.outbox.front() != Some(event) {
            return Ok(());
        }
        self.outbox.pop_front();
        self.acknowledged = self.acknowledged.saturating_add(1);
        self.last_acknowledged_type = Some(event.kind.clone());
        if event.duplicate {
            self.duplicates_sent = self.duplicates_sent.saturating_add(1);
        } else if duplicate {
            let id = self.next_id()?;
            self.outbox.push_front(SecurityEvent {
                id,
                duplicate: true,
                ..event.clone()
            });
        }
        Ok(())
    }

    /// The CSMS refused the event with a non-transient CALLERROR: it is not retried.
    pub(super) fn reject(&mut self, event: &SecurityEvent) {
        if self.outbox.front() == Some(event) {
            self.outbox.pop_front();
            self.rejected = self.rejected.saturating_add(1);
        }
    }
}
