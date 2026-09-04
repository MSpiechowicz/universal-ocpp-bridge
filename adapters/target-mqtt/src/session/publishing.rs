use rumqttc::PublishOptions;
use serde::{Serialize, de::DeserializeOwned};
use uob_application::{DeliveryOutcome, TargetDelivery, TargetHealthState};

use super::{PublishPurpose, Session, TrackedPurpose};
use crate::{error::permanent_mapping, mapping::WirePublication};

impl<E, P> Session<E, P>
where
    E: Serialize + Send + Sync + 'static,
    P: Clone + DeserializeOwned + Send + 'static,
{
    pub(super) fn accept_delivery(&mut self, delivery: &TargetDelivery<E>) {
        let delivery_id = delivery.delivery_id.clone();
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
            self.emit_health(TargetHealthState::Degraded, "mqtt.retained_state_capacity");
            self.spawn_report(
                delivery_id,
                DeliveryOutcome::RetryableFailure {
                    reason: "mqtt.retained_state_capacity".to_owned(),
                },
            );
            return;
        }
        if publication.retain {
            self.retained_state
                .insert(publication.topic.clone(), publication.clone());
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
