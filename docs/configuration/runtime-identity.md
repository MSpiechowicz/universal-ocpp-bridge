# Runtime identity configuration

The service composition root owns bridge, environment, release, process, and selected-target
identity. Network requests cannot supply or override these values. `RuntimeIdentity` is attached to
events, exports, and diagnostics, while `GET /api/v1/identity` exposes the complete trusted service
context. A service with no selected target still exposes this endpoint and does not construct or
enable MQTT.

Production is the default environment. A production deployment should supply configuration owned
by the service account and release metadata verified from the installed immutable artifact:

```toml
[bridge]
id = "site-01"
environment = "production"
target_id = "main"

[release]
id = "1.0.0-rc.1"
digest = "sha256:<verified-artifact-digest>"
```

Staging and demo instances must be explicit and use distinct bridge identities, listeners, data,
credentials, and targets:

```toml
[bridge]
id = "site-01-staging"
environment = "staging"
target_id = "staging-http"

[release]
id = "1.0.0-rc.1"
digest = "sha256:<verified-candidate-digest>"
```

```toml
[bridge]
id = "local-demo"
environment = "demo"
# No target_id: management API only.

[release]
id = "development"
digest = "sha256:<local-build-digest>"
```

Startup rejects a target selection whose bridge or environment differs from the service identity.
Every process invocation generates a new UUID process identity; bridge, environment, and release
identity remain stable until their trusted configuration or installed artifact changes.

## Demo native local authorization provisioning

Independent station options get_local_list_version/send_local_list/clear_cache support
exact ocpp16j and ocpp201 roster editions, still default-off and demo-only behind
separate privileged grants. The existing charging.local_authorization_updates_file
is an owner-only startup file, not an inline configuration list or public API.
Its station_id/update_reference/expires_at/request entries route list16 capabilities
only to native16 providers and list201 capabilities only to native201 providers.
Exact station-root scope, version, update type and expiry must match the requested
envelope; station editions cannot share an undifferentiated provider.

No environment, target or configured enabled flag implies permission, supported
hardware, learned device facts or installed contents. Production remains rejected.
See [the full operator example](../operations/headless-cli.md#protected-ocpp-201-station-authorization-list-and-cache)
and [native security boundaries](../security/local-authorization.md#station-side-ocpp-201-list-and-cache).

Independent station options reserve_now/cancel_reservation (plus the separate
reserve_connector_zero_supported) are likewise default-off and demo-only for exact
ocpp16j stations. reserve_now additionally needs a per-station owner-only
reservation16_file of protected reservation references and native group facts; see
[the operator example](../operations/headless-cli.md#protected-ocpp-16-reservations).
For exact ocpp201 stations the same two options are independently default-off and
demo-only, reserve_now needs a reservation201_file, and unspecified-EVSE reservations
additionally need reserve_non_evse_specific_supported; see
[the 2.0.1 operator example](../operations/headless-cli.md#protected-ocpp-201-reservations).

Independent station options update_firmware/signed_update_firmware are default-off and
demo-only for exact ocpp16j stations, mutually exclusive per station, and need a bounded
firmware_job_timeout_seconds. Either one requires a [charging.firmware] section whose
owner-only catalog_file and private spool_directory feed a loopback test artifact service
and a freshly generated TEST ONLY PKI; nothing in it is production artifact storage. See
[the operator example](../operations/headless-cli.md#protected-ocpp-16-firmware-updates).

For exact ocpp201 stations, update_firmware enables OCPP 2.0.1 UpdateFirmware with the same
[charging.firmware] section and bounded firmware_job_timeout_seconds. It is a secure update
(L01) that only sends signed artifacts unless non_secure_firmware selects L02, which only
sends unsigned ones. signed_update_firmware is refused for ocpp201 stations, and
non_secure_firmware is refused without update_firmware or on ocpp16j stations. See
[the 2.0.1 operator example](../operations/headless-cli.md#protected-ocpp-201-firmware-updates).

Independent station options get_diagnostics/get_log enable OCPP 1.6 GetDiagnostics and the
Security Whitepaper GetLog for exact ocpp16j stations only. They are default-off and demo-only,
need a bounded diagnostics_job_timeout_seconds, accept an optional diagnostics_upload_max_bytes,
and use the same [charging.firmware] artifact service, which then needs no catalog_file unless a
station also enables firmware. Timeout and cap options without either action are refused. See
[the operator example](../operations/headless-cli.md#protected-ocpp-16-diagnostics-and-log-uploads).
