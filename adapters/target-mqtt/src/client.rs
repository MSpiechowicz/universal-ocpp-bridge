use rumqttc::{
    AsyncClient, Broker, EventLoop, LastWill, MqttOptions, NetworkOptions, QoS, Transport,
};

use crate::{configuration::ResolvedCredentials, target::MqttTarget};

const MAX_MQTT_TOPIC_BYTES: usize = 65_535;
const MAX_PUBLISH_FRAMING_BYTES: usize = 9;
const MAX_INCOMING_CONTROL_PACKET_BYTES: usize = 64 * 1024;

pub(crate) fn create_client(
    target: &MqttTarget,
    credentials: Option<ResolvedCredentials>,
) -> (AsyncClient, EventLoop) {
    let offline = target
        .topics
        .availability_publication(&target.settings.target_instance_id, false);
    let broker = Broker::tcp(
        target.settings.endpoint.host.clone(),
        target.settings.endpoint.port,
    );
    let mut options = MqttOptions::new(&target.settings.client_id, broker);
    options
        .set_keep_alive(
            target
                .runtime
                .keep_alive
                .as_secs()
                .try_into()
                .expect("validated MQTT keepalive"),
        )
        .set_clean_session(false)
        .set_inflight(
            u16::try_from(target.runtime.maximum_in_flight_deliveries)
                .expect("validated MQTT in-flight bound"),
        )
        .set_request_channel_capacity(target.runtime.request_capacity)
        .set_max_packet_size(
            MAX_INCOMING_CONTROL_PACKET_BYTES,
            maximum_outgoing_packet_bytes(target.runtime.maximum_message_bytes),
        )
        .set_last_will(LastWill::new(
            offline.topic,
            offline.payload,
            QoS::AtLeastOnce,
            true,
        ));
    configure_transport(&mut options, target.settings.endpoint.tls, credentials);
    // The root rumqttc client API is MQTT 3.1.1 (Protocol::V4); MQTT 5 is a separate module.
    let (client, mut eventloop) = AsyncClient::builder(options)
        .capacity(target.runtime.request_capacity)
        .build();
    let mut network = NetworkOptions::new();
    network
        .set_connection_timeout(target.runtime.connection_timeout_seconds)
        .set_tcp_nodelay(true);
    eventloop.set_network_options(network);
    (client, eventloop)
}

const fn maximum_outgoing_packet_bytes(maximum_payload_bytes: usize) -> usize {
    maximum_payload_bytes.saturating_add(MAX_MQTT_TOPIC_BYTES + MAX_PUBLISH_FRAMING_BYTES)
}

fn configure_transport(
    options: &mut MqttOptions,
    tls: bool,
    credentials: Option<ResolvedCredentials>,
) {
    if let Some(credentials) = credentials {
        if let Some((username, password)) = credentials.login {
            options.set_credentials(username, password);
        }
        if tls {
            match credentials.certificate_authority {
                Some(authority) => {
                    options.set_transport(Transport::tls(
                        authority,
                        credentials.client_authentication,
                        None,
                    ));
                }
                None => {
                    options.set_transport(Transport::tls_with_default_config());
                }
            }
        }
    } else if tls {
        options.set_transport(Transport::tls_with_default_config());
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_MQTT_TOPIC_BYTES, MAX_PUBLISH_FRAMING_BYTES, maximum_outgoing_packet_bytes};

    #[test]
    fn outgoing_bound_includes_the_largest_topic_and_protocol_framing() {
        assert_eq!(
            maximum_outgoing_packet_bytes(123),
            123 + MAX_MQTT_TOPIC_BYTES + MAX_PUBLISH_FRAMING_BYTES
        );
        assert_eq!(maximum_outgoing_packet_bytes(usize::MAX), usize::MAX);
    }
}
