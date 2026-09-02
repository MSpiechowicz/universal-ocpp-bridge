use std::time::Duration;

use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;

use crate::{Action, ExpectedFrame, Observation, Payload, Peer, PeerConfig, Scenario};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StepResult {
    pub id: String,
    pub outcome: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunFailure {
    pub step: String,
    pub code: &'static str,
    pub message: &'static str,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RunReport {
    pub steps: Vec<StepResult>,
    pub observations: Vec<Observation>,
    pub failure: Option<RunFailure>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ScenarioRunner;

impl ScenarioRunner {
    pub async fn run(&self, scenario: &Scenario) -> RunReport {
        let mut report = RunReport::default();
        let mut peer = None;
        for step in &scenario.steps {
            let execution = execute(scenario, step, &mut peer);
            let result = timeout(Duration::from_millis(step.timeout_ms), execution).await;
            let failure = match result {
                Ok(Ok(())) => {
                    report.steps.push(StepResult {
                        id: step.id.clone(),
                        outcome: "passed",
                    });
                    if let Some(connected) = &peer {
                        report.observations = connected.observations();
                    }
                    if matches!(step.action, Action::Disconnect) {
                        peer = None;
                    }
                    None
                }
                Ok(Err((code, message))) => Some(RunFailure {
                    step: step.id.clone(),
                    code,
                    message,
                }),
                Err(_) => Some(RunFailure {
                    step: step.id.clone(),
                    code: "step_timeout",
                    message: "adversarial scenario step exceeded its timeout",
                }),
            };
            if let Some(failure) = failure {
                report.steps.push(StepResult {
                    id: step.id.clone(),
                    outcome: "failed",
                });
                report.failure = Some(failure);
                break;
            }
        }
        report
    }
}

async fn execute(
    scenario: &Scenario,
    step: &crate::Step,
    peer: &mut Option<Peer>,
) -> Result<(), (&'static str, &'static str)> {
    match step.action {
        Action::Connect => {
            if peer.is_some() {
                return Err(("already_connected", "peer is already connected"));
            }
            *peer = Some(
                Peer::connect(PeerConfig {
                    endpoint: scenario.endpoint.clone(),
                    subprotocol: scenario.subprotocol.clone(),
                    max_outbound_bytes: scenario.max_outbound_bytes,
                    max_inbound_bytes: scenario.max_inbound_bytes,
                    observation_capacity: scenario.observation_capacity,
                    authorization: None,
                })
                .await
                .map_err(|_| ("connection_failed", "bare WebSocket connection failed"))?,
            );
        }
        Action::Send => {
            let connected = peer
                .as_mut()
                .ok_or(("not_connected", "peer is not connected"))?;
            match step.payload.as_ref().expect("validated send payload") {
                Payload::Text { value } => connected.send_text(value.clone()).await,
                Payload::BinaryHex { value } => connected.send_binary(decode_hex(value)).await,
                Payload::RepeatedText { byte, bytes } => {
                    connected.send_text(byte.to_string().repeat(*bytes)).await
                }
            }
            .map_err(|_| ("send_failed", "raw frame could not be sent"))?;
        }
        Action::Receive => {
            let connected = peer
                .as_mut()
                .ok_or(("not_connected", "peer is not connected"))?;
            let expected = step.expect.as_ref().expect("validated receive expectation");
            if matches!(expected, ExpectedFrame::Error) {
                if connected.receive().await.is_ok() {
                    return Err((
                        "expected_error",
                        "peer received a frame instead of an error",
                    ));
                }
            } else {
                let message = connected
                    .receive()
                    .await
                    .map_err(|_| ("receive_failed", "expected frame was not received"))?;
                assert_message(&message, expected)?;
            }
        }
        Action::Delay => {
            tokio::time::sleep(Duration::from_millis(
                step.delay_ms.expect("validated delay"),
            ))
            .await;
        }
        Action::Disconnect => {
            let connected = peer
                .as_mut()
                .ok_or(("not_connected", "peer is not connected"))?;
            connected
                .disconnect()
                .await
                .map_err(|_| ("disconnect_failed", "WebSocket close handshake failed"))?;
        }
    }
    Ok(())
}

fn assert_message(
    message: &Message,
    expected: &ExpectedFrame,
) -> Result<(), (&'static str, &'static str)> {
    match (message, expected) {
        (
            Message::Text(actual),
            ExpectedFrame::Text {
                contains,
                json_pointer,
                equals,
            },
        ) => {
            if contains.as_ref().is_some_and(|part| !actual.contains(part)) {
                return Err((
                    "text_mismatch",
                    "text frame did not contain the expected value",
                ));
            }
            if let (Some(pointer), Some(expected_value)) = (json_pointer, equals) {
                let document: serde_json::Value = serde_json::from_str(actual)
                    .map_err(|_| ("invalid_response_json", "text frame is not valid JSON"))?;
                if document.pointer(pointer) != Some(expected_value) {
                    return Err(("json_path_mismatch", "JSON field path did not match"));
                }
            }
            Ok(())
        }
        (Message::Binary(actual), ExpectedFrame::Binary { bytes })
            if bytes.is_none_or(|expected_bytes| expected_bytes == actual.len()) =>
        {
            Ok(())
        }
        (Message::Close(actual), ExpectedFrame::Close { code })
            if code.is_none_or(|expected_code| {
                actual
                    .as_ref()
                    .is_some_and(|frame| u16::from(frame.code) == expected_code)
            }) =>
        {
            Ok(())
        }
        _ => Err((
            "frame_mismatch",
            "received WebSocket frame did not match expectation",
        )),
    }
}

fn decode_hex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let high = hex_digit(pair[0]);
            let low = hex_digit(pair[1]);
            high * 16 + low
        })
        .collect()
}

const fn hex_digit(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        b'A'..=b'F' => byte - b'A' + 10,
        _ => 0,
    }
}
