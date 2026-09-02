# OCPP WebSocket endpoint

The protocol adapter owns the charger-facing Axum endpoint at `/ocpp/{station_id}`. It accepts only
WebSocket upgrades that negotiate `ocpp1.6` or `ocpp2.0.1`; target selection is deliberately absent
from this boundary. An unavailable MQTT, EMS HTTP, or EMS MQTT target therefore cannot prevent a
safe local charger admission.

## Admission order

For production WSS listeners, the adapter completes the configured rustls handshake before HTTP
extraction. Mutual-TLS mode verifies the client chain and passes its end-entity certificate to the
station authenticator. The endpoint then:

1. selects one exact supported WebSocket subprotocol;
2. parses the path as the claimed station identity;
3. requires HTTP Basic authentication whose username exactly matches that path identity;
4. authenticates the password and optional certificate binding against the local allowlist;
5. atomically reserves the station identity and a shared connected-station slot;
6. reserves one bounded socket-handoff slot before returning the upgrade response.

Authentication failures use one generic external message and do not reveal whether the identity,
credential, or certificate binding failed. Unsupported protocols, duplicate live identities,
station-capacity exhaustion, and handoff-capacity exhaustion are rejected before upgrade. Dropping
an accepted connection releases both duplicate ownership and the shared resource reservation, so a
station can reconnect safely.

The password is decoded only long enough for the existing constant-time credential comparison. It
is not retained in the accepted connection or included in errors and diagnostics.

## Transport and resource policy

`OcppEndpoint::serve_tls` accepts an already bound configurable TCP listener and the validated TLS
acceptor. `OcppEndpoint::serve_plaintext` checks the trusted application runtime identity and starts
only when the environment is explicitly `demo`; callers cannot enable plaintext by supplying a
separate environment value.

The endpoint copies the immutable maximum OCPP message size from the application's shared runtime
budget into Axum's WebSocket frame and message limits. The default is 256 KiB. Active sockets hold
the same shared admission guard used by station actors, enforcing the default limit of 16 connected
stations across the process. Accepted sockets are transferred through a bounded single-consumer
queue to the later OCPP call-lifecycle integration; queue saturation closes admission rather than
creating an unbounded task or payload backlog.

Real-socket tests cover both supported WSS subprotocols with credentials and mutual TLS, generic
handshake rejection, duplicate and 17th-station rejection, oversized messages, demo-only
plaintext, and admission under each selected target profile while that target is reconnecting.
