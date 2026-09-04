use std::collections::BTreeMap;

use rumqttc::PublishOptions;
use serde::{Serialize, de::DeserializeOwned};
use uob_application::{DeliveryOutcome, TargetDelivery, TargetHealthState, TargetMessage};
use uob_contracts::StationSnapshot;

use super::{PublishPurpose, Session, TrackedPurpose};
use crate::{error::permanent_mapping, mapping::WirePublication};

/// Retained publications derived from one canonical snapshot beside its own state document.
struct DerivedPublications {
    discovery: Vec<WirePublication>,
    point_catalog: Vec<WirePublication>,
}

impl<E, P> Session<E, P>
where
    E: Serialize + Send + Sync + 'static,
    P: Clone + DeserializeOwned + Send + 'static,
{
    pub(super) fn accept_delivery(&mut self, delivery: &TargetDelivery<E>) {
        let delivery_id = delivery.delivery_id.clone();
        let snapshot = match delivery.message.as_ref() {
            TargetMessage::StationSnapshot(snapshot) => Some(snapshot),
            _ => None,
        };
        let derived = match self.derive(snapshot) {
            Ok(derived) => derived,
            Err(reason) => {
                self.spawn_report(delivery_id, reason);
                return;
            }
        };
        if !Self::has_cache_headroom(
            &derived.discovery,
            &self.discovery,
            self.runtime.discovery_capacity,
        ) {
            self.refuse_for_capacity(delivery_id, "mqtt.discovery_capacity");
            return;
        }
        if !Self::has_cache_headroom(
            &derived.point_catalog,
            &self.point_catalog,
            self.runtime.point_catalog_capacity,
        ) {
            self.refuse_for_capacity(delivery_id, "mqtt.point_catalog_capacity");
            return;
        }
        let publication = match self.topics.map(
            &self.settings.target_instance_id,
            self.settings.configuration_revision,
            delivery,
            self.runtime.maximum_message_bytes,
        ) {
            Ok(publication) => publication,
            Err(error) => {
                self.spawn_report(delivery_id, permanent_mapping(error));
                return;
            }
        };
        if publication.retain
            && !self.retained_state.contains_key(&publication.topic)
            && self.retained_state.len() == self.runtime.retained_state_capacity
        {
            self.refuse_for_capacity(delivery_id, "mqtt.retained_state_capacity");
            return;
        }
        if publication.retain {
            self.retained_state
                .insert(publication.topic.clone(), publication.clone());
        }
        for discovery in derived.discovery {
            self.discovery
                .insert(discovery.topic.clone(), discovery.clone());
            self.replay.push_back(discovery);
        }
        for point in derived.point_catalog {
            self.point_catalog
                .insert(point.topic.clone(), point.clone());
            self.replay.push_back(point);
        }
        if self.queue_publication(&publication, PublishPurpose::Delivery(delivery_id.clone())) {
            return;
        }
        self.spawn_report(
            delivery_id,
            DeliveryOutcome::RetryableFailure {
                reason: "mqtt.request_capacity".to_owned(),
            },
        );
    }

    fn derive(
        &self,
        snapshot: Option<&StationSnapshot>,
    ) -> Result<DerivedPublications, DeliveryOutcome> {
        let Some(snapshot) = snapshot else {
            return Ok(DerivedPublications {
                discovery: Vec::new(),
                point_catalog: Vec::new(),
            });
        };
        let discovery = if self.settings.home_assistant_discovery {
            self.topics
                .home_assistant_discovery(snapshot, self.runtime.maximum_message_bytes)
                .map_err(permanent_mapping)?
        } else {
            Vec::new()
        };
        let point_catalog = if self.settings.profile.publishes_point_catalog() {
            self.topics
                .point_catalog(snapshot, self.runtime.maximum_message_bytes)
                .map_err(permanent_mapping)?
        } else {
            Vec::new()
        };
        Ok(DerivedPublications {
            discovery,
            point_catalog,
        })
    }

    fn has_cache_headroom(
        derived: &[WirePublication],
        cache: &BTreeMap<String, WirePublication>,
        capacity: usize,
    ) -> bool {
        let additions = derived
            .iter()
            .filter(|publication| !cache.contains_key(&publication.topic))
            .count();
        cache.len().saturating_add(additions) <= capacity
    }

    fn refuse_for_capacity(
        &mut self,
        delivery_id: uob_application::DeliveryId,
        reason: &'static str,
    ) {
        self.emit_health(TargetHealthState::Degraded, reason);
        self.spawn_report(
            delivery_id,
            DeliveryOutcome::RetryableFailure {
                reason: reason.to_owned(),
            },
        );
    }

    pub(super) fn flush_replay(&mut self) {
        while self.connected && self.awaiting_packet_id.len() < self.runtime.request_capacity {
            let Some(publication) = self.replay.pop_front() else {
                break;
            };
            if !self.queue_publication(&publication, PublishPurpose::Internal) {
                self.replay.push_front(publication);
                break;
            }
        }
    }

    pub(super) fn queue_publication(
        &mut self,
        publication: &WirePublication,
        purpose: PublishPurpose,
    ) -> bool {
        if self
            .client
            .try_publish(
                publication.topic.clone(),
                publication.payload.clone(),
                PublishOptions::at_least_once().retain(publication.retain),
            )
            .is_err()
        {
            return false;
        }
        self.awaiting_packet_id.push_back(TrackedPurpose {
            epoch: self.epoch,
            purpose,
        });
        self.ensure_progress_deadline();
        true
    }
}
