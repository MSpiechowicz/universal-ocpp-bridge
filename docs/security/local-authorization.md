# Local authorization policy

The application owns charging authorization. A station, management client, selected target, browser,
or payment test provider cannot declare an identity authorized by placing a token, principal, or
success flag in a payload.

## Sensitive input and persisted references

`SensitiveAuthorizationToken` deliberately implements neither `Debug` nor `Display` and clears its
owned bytes on drop. The production-capable `local.sha256` provider maps those bytes to an opaque
`sha256:<digest>` reference. Only that reference is compared with or stored in the local allowlist;
raw tokens are not placed in SQLite, events, command results, diagnostics, or provider errors.

Provisioning must calculate the reference using the exact token bytes delivered by the station. A
local allowlist entry binds that reference to a canonical station or child resource, a monotonic
revision, active or revoked state, a trusted change time, and an optional expiry. Equal revisions
are idempotent only when every field is identical. Stale or conflicting changes fail closed.

## Decision and recovery behavior

The decision order is explicit: unknown reference, revoked state, expiry, then canonical resource
scope. Expiry is inclusive, so a reference is denied when trusted UTC time is equal to or later than
`expires_at`. A station-level entry includes that station's EVSEs or connectors; it never crosses a
bridge or station boundary.

Changes commit through the application-owned atomic operational-store transaction before becoming
visible in memory. Startup restores the latest revisions from SQLite before accepting authorization
work. The local provider and policy perform no DNS, target, internet, broker, or external-database
operation, so those outages do not change an otherwise valid local decision.

Both station authorization handlers and start-command admission use `LocalAuthorizationService`.
The command guard treats a payload-supplied authorization reference only as a lookup key and denies
unknown, expired, revoked, or out-of-scope values before the common command port. Management and
target adapters must still attach their authenticated origin outside the request body and pass the
existing scoped access guard; transport authentication and local charging authorization are
separate, cumulative checks.

## Environment restrictions

Authorization providers declare whether they are test-only. The runtime security policy rejects
every test-only authorization provider in `production`, using the trusted process environment rather
than request or configuration payload claims. Staging and demo may select an explicitly configured
test provider; `local.sha256` is not test-only.
