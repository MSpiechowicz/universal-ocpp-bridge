use rumqttc::Publish;
use serde::{Serialize, de::DeserializeOwned};
use tokio::task::JoinError;
use uob_application::{
    CommandAdmissionError, CommandAdmissionErrorCode, TargetError, TargetHealthState,
};
use uob_contracts::CommandResult;

use super::{PublishPurpose, Session};
use crate::{
    error::permanent_data,
    ingress::{Ingress, IngressContext, admission_rejection, classify},
};

impl<E, P> Session<E, P>
where
    E: Serialize + Send + Sync + 'static,
    P: Clone + DeserializeOwned + Send + 'static,
{
    pub(super) fn handle_command(&mut self, publication: &Publish) {
        let Ok(topic) = std::str::from_utf8(&publication.topic) else {
            self.emit_health(TargetHealthState::Degraded, "mqtt.command_topic_invalid");
            return;
        };
        match classify::<P>(
            IngressContext {
                topics: &self.topics,
                target_instance_id: &self.settings.target_instance_id,
                principal: &self.settings.command_principal,
                maximum_command_bytes: self
                    .runtime
                    .maximum_message_bytes
                    .min(self.context.limits.maximum_command_bytes),
            },
            topic,
            &publication.payload,
            publication.retain,
        ) {
            Ingress::Submit(command) if self.commands.len() < self.effective_command_limit() => {
                let admission = std::sync::Arc::clone(&self.context.commands);
                let rejection_context = command.clone();
                self.commands.spawn(async move {
                    match admission.submit(command).await {
                        Ok(result) => result,
                        Err(error) => admission_rejection(&rejection_context, &error),
                    }
                });
            }
            Ingress::Submit(command) => {
                let error = CommandAdmissionError::new(
                    CommandAdmissionErrorCode::Busy,
                    "mqtt.command_capacity",
                );
                self.accept_command_result(&admission_rejection(&command, &error));
            }
            Ingress::Reject(result) => self.accept_command_result(&result),
            Ingress::Ignore(reason) => self.emit_health(TargetHealthState::Degraded, reason),
        }
    }

    pub(super) fn flush_command_results(&mut self) {
        while self.connected && self.awaiting_packet_id.len() < self.runtime.request_capacity {
            let Some(publication) = self.command_results.pop_front() else {
                break;
            };
            if !self.queue_publication(&publication, PublishPurpose::Internal) {
                self.command_results.push_front(publication);
                break;
            }
        }
    }

    pub(super) fn finish_command_task(
        &mut self,
        result: Option<Result<CommandResult, JoinError>>,
    ) -> Result<(), TargetError> {
        match result {
            Some(Ok(result)) => {
                self.accept_command_result(&result);
                Ok(())
            }
            Some(Err(_)) => Err(permanent_data("mqtt.command_task_failed")),
            None => Ok(()),
        }
    }

    fn accept_command_result(&mut self, result: &CommandResult) {
        let publication = match self.topics.command_result(
            &self.settings.target_instance_id,
            result,
            self.runtime.maximum_message_bytes,
        ) {
            Ok(publication) => publication,
            Err(error) => {
                self.emit_health(TargetHealthState::Degraded, error.reason());
                return;
            }
        };
        let maximum = self.effective_command_limit().max(1);
        if self.command_results.len() >= maximum {
            self.emit_health(TargetHealthState::Degraded, "mqtt.command_result_capacity");
            return;
        }
        self.command_results.push_back(publication);
    }

    fn effective_command_limit(&self) -> usize {
        self.runtime
            .maximum_in_flight_commands
            .min(self.context.limits.maximum_in_flight_commands)
    }
}
