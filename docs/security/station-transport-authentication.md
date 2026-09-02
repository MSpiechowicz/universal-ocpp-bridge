# Station transport authentication

The protocol adapter exposes the fail-closed transport boundary used before an OCPP WebSocket can
allocate station state or reach application workflows. It is independent of the selected bridge
target and supports both `ocpp1.6` and `ocpp2.0.1`.

## Configuration and secret resolution

Every listener configuration contains TLS certificate/key references and a nonempty local station
allowlist. Every station entry contains a stable station ID and a distinct credential reference.
Mutual-TLS entries additionally contain a SHA-256 binding for that station's end-entity client
certificate, and the listener contains a client-CA reference.

Offline validation rejects:

- an empty allowlist or duplicate station ID;
- shared credential references;
- missing or ignored client-CA and certificate-binding fields;
- one client-certificate binding assigned to multiple stations.

The composition root resolves credential references without placing secret values in the safe
configuration model. A resolved station credential must contain at least 16 bytes of unpredictable
material. It is immediately reduced to a one-way digest, compared in constant time, and never
included in `Debug` or error output. Runtime construction rejects missing, extra, duplicate, or
shared resolved credentials, so two stations cannot intentionally or accidentally share a secret.
These credentials are high-entropy machine secrets, not human passwords; deployment tooling should
generate at least 128 random bits and protect the referenced files as secrets.

## Admission sequence

Admission proceeds in this order:

1. `rustls` completes the TLS handshake using the configured server certificate and key.
2. In mutual-TLS mode, `rustls` requires a client certificate and verifies its chain, validity
   period, and client-authentication usage against the configured trust anchors.
3. The endpoint selects one supported OCPP WebSocket subprotocol and supplies the station path ID,
   transport credential, and validated peer chain to `StationAuthenticator`.
4. The authenticator finds the preconfigured identity, verifies its unique credential, and, for
   mutual TLS, compares the end-entity certificate with that station's configured binding.
5. Only the returned `AuthenticatedStation` ID and protocol may be used to create station runtime
   state. Raw path and header values remain untrusted.

`Credential` mode still requires TLS and the unique station secret but does not request a client
certificate. `CredentialAndMutualTls` requires both factors. There is deliberately no plaintext or
anonymous production acceptor in this boundary.

Certificate renewal changes the end-entity DER fingerprint. Deployments using mutual TLS must
update the station binding through their reviewed configuration rollout while retaining the proper
issuing CA. This item does not implement OCPP certificate-lifecycle messages or a production PKI
provider.

## Safe failures and verification

Configuration failures expose a stable category and safe field name. Station-admission errors keep
specific categories for trusted policy handling, while their external display text is always the
same generic denial. TLS handshake failures similarly discard library and certificate details at
the adapter boundary.

Focused tests establish real TLS and WebSocket sessions for both supported OCPP subprotocols. They
also prove that unknown identities, absent and invalid credentials, cross-station credential reuse,
wrong certificate bindings, missing client certificates, untrusted issuers, and expired client
certificates fail closed. The authenticated WebSocket route and station-task integration remain the
separate endpoint responsibility.
