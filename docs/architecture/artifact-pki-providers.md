# Artifact and PKI provider ports with local test providers

Firmware, diagnostics, security-event and certificate workflows depend on two application-owned
ports in `uob_application`. They never import a concrete provider:

- `artifact_provider::ArtifactProvider` resolves an operator-chosen `ArtifactReference` to a
  station-reachable `ArtifactLocation` plus `ArtifactIntegrity`: the exact size, a SHA-256 digest
  and, for signed firmware, the signing certificate and base64 signature. It also opens bounded
  `UploadDestination`s for diagnostics and security logs and reports each one's `UploadStatus`.
- `certificate_provider::CertificateProvider` verifies and signs station CSRs for the expected
  common name, supplies installable roots per `TrustAnchorKind`, and returns a `TrustDecision` for an
  untrusted chain and `ChainPurpose` at a given instant.

Both ports are object-safe, return boxed `Send` futures, and describe themselves with a
`{ kind, test_only }` descriptor, like `AuthorizationProvider`. Every value that crosses them is
bounded by OCPP 2.0.1 message limits: locations are capped at 512 bytes, signatures at 800 bytes,
certificates and CSRs at 5 500 bytes, and chains at 10 000 bytes and five certificates. Only public
material crosses: PEM values carrying any `PRIVATE KEY` block are refused at construction.
Locations must be `http`, `https`, `ftp` or `ftps` URLs without embedded user credentials, because
they are sent to stations and appear in diagnostics. Errors and decisions are closed enums without
payloads, paths or certificate text.

Upload locations end with `/`, so a station appends the file name it reported (OCPP 2.0.1
N01.FR.21). A refused attempt (`TooLarge`, `TimedOut`, `Interrupted` or `Unavailable`) leaves the
destination open for retry until one upload is `Received`.

## Production guard

`RuntimeSecurityPolicy` mirrors the authorization-provider guard:

- `authorize_artifact_provider` and `authorize_certificate_provider` reject test-only providers in
  production.
- `authorize_provider_material` rejects test-only descriptors, destinations, roots and signed
  certificates in production. This catches material created in an isolated environment before
  production can send it to a station or trust it.

Staging and demo environments accept both. The test providers call these guards from their
constructors, so a production composition cannot create them.

## Local test providers

`uob_provider_adapter::test_ca::TestCertificateAuthority` generates fresh in-memory hierarchies on
every construction:

- A CSO root with an issuing CA (path length 0). It signs station certificates with `clientAuth`
  and returns a leaf-first chain that includes the issuing CA.
- A manufacturer root that certifies a 3072-bit RSA firmware signer. Firmware signatures use
  RSA-PSS with SHA-256 over the complete image (L01.FR.04).
- A V2G root that signs V2G certificates directly. No MO root is provided.

Every CA subject contains `TEST ONLY`. A CSR must carry a valid self-signature, an ECDSA P-256 or
P-384 or an RSA key of at least 2048 bits, the expected common name, and the configured CSO
organization (A00.FR.509, A00.FR.511). Certificates use CA-chosen extensions: requested extensions
are never copied, and requests with unsupported ones are refused. Chain decisions use
`rustls-webpki` against the root for the purpose. Station leaves also need the CSO organization
and, when provided, the expected common name (A00.FR.404, A00.FR.405).

`uob_provider_adapter::test_artifacts::TestArtifactService` (Unix only) publishes bounded firmware
in memory and serves it over HTTP at `{public_base}/artifacts/{reference}`. Downloads stream
through `ArtifactTransfers`, so they share admission, buffer and deadline bounds. Uploads are
accepted with PUT or POST (N01.FR.18), with or without an appended file name, at
`{public_base}/uploads/{id}/`. A declared `Content-Length` over the cap is refused before any bytes
are read. Chunked bodies are refused while streaming. The bytes stream through the same transfers
into unlinked spool files; the digest is computed over exactly the bytes stored. Destinations are
bounded, and the oldest idle one is evicted when full. HTTP error responses have empty bodies.

## Fault controls

| Provider | Control | Effect |
|---|---|---|
| Both | `set_unavailable` | Port operations return `Unavailable`; HTTP transfers return 503 |
| Both | `set_delay` | Every port operation and HTTP transfer waits before admission |
| Artifacts | `set_corrupt_downloads` | Served bytes no longer match the digest and signature |
| Artifacts | `set_upload_cap` | Uploads above the cap are refused with 413 and `TooLarge` |
| PKI | `set_reject_csr` | CSRs are refused with `PolicyRejected` |

## Simulator transfers

`uob_sim::artifact_transfer` is the simulator's independent station-side client. `download` and
`upload` accept only `http`/`https` locations without credentials, follow no redirects or proxies,
hold at most `maximum_bytes` and finish within one deadline. The file name is appended to directory
locations. Failures are sanitized and never echo the location. The simulator does not depend on
the provider adapter or any service crate.

## Verification

```text
cargo test --locked -p uob-application --lib artifact_provider certificate_provider security
cargo test --locked -p uob-provider-adapter --test provider_contract
cargo test --locked -p uob-sim --test artifact_transfer
```

The reusable contract functions in `adapters/providers/tests/provider_contract/contract.rs` accept
any `ArtifactProvider` and `CertificateProvider`. They check firmware resolution, upload
destinations, firmware signatures verified independently against the installable manufacturer
root, CSR subject policy and root bounds. The network tests run the simulator client against the
served test provider over loopback sockets: a signed download and a diagnostics upload, corrupt and
unavailable faults, uploads over the cap refused by header and while chunked, and a stalled upload
timing out. Further tests cover production refusal, the `TEST ONLY` marking, and the absence of
private keys, credentials and spool paths in responses, errors and debug output.

The OCPP 1.6J and OCPP 2.0.1 firmware workflows ([ocpp16-firmware.md](ocpp16-firmware.md),
[ocpp201-firmware.md](ocpp201-firmware.md)) consume both ports: the demo charging runtime
composes one `TestArtifactService` and one `TestCertificateAuthority` from
`[charging.firmware]` and shares them between stations of either edition.

The OCPP 1.6J diagnostics and log workflow ([ocpp16-diagnostics.md](ocpp16-diagnostics.md))
opens upload destinations on the same `TestArtifactService` and checks each reported upload
against `upload_status`.

The OCPP 2.0.1 log workflow ([ocpp201-diagnostics.md](ocpp201-diagnostics.md)) does the same
for `GetLog` on 2.0.1 stations.

Not in scope: security-event and certificate CSMS workflows, compose integration, and
production PKI or persistent artifact storage.
