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

## Station-side OCPP 1.6 list and cache

The demo station controls `GetLocalListVersion`, `SendLocalList` and `ClearCache`
manage the charge point's **separate native list/cache**, not the service's
`local.sha256` allowlist. Each is independently default-off, station-scoped and
requires the existing privileged grant. Ordinary control and target credentials
do not gain native list authority.

The service's policy still hashes exact original token bytes. Native update
validation and the independent simulator use full Unicode casefold without extra
normalization, preserving original wire spelling, status, expiry and private parent.
Changing the station list or clearing its cache does not provision, revoke or
case-normalize service-side authorization.
The normal incoming OCPP 1.6 Authorize decoded-call path resolves the existing
`local.sha256` provider under the same current durable policy and timeout boundary.
Independent native socket evidence confirmed exact provisioned uppercase bytes earn
Accepted while a casefold-equivalent original identity earns Invalid; simulator
list/cache updates and ClearCache do not change that service policy.

Public `SendLocalList` commands use
`urn:uob:ocpp16:SendLocalListReference:1` with only `listVersion`, `updateType`
and `updateReference`. An immutable owner-only startup file binds each expiring
`list16:` capability to exact station resource, version, type and native material.
Even absent/empty native updates require a capability; inline entries and parent
identities are rejected. Queued/persisted command envelopes contain references,
not native entries. No public provisioning/revocation API or hot reload exists.

Admission and the first native send poll recheck scope, expiry, revocation, live
socket generation and byte/count limits. Caps are 128 capabilities, 256 entries
per update, 64 KiB encoded native update, 1 MiB retained material plus metadata,
and a 2 MiB provisioning file. Smaller validated session limits also apply;
unknown limits remain unknown and learned facts reset on reconnect. A Full count
does not establish a Differential update's final station cardinality.

Optional `local_authorization_16` result evidence contains exact native status
and safe version/update metadata only. A query reports `0` for an empty list and
`-1` for unsupported lists; updates forbid precisely `-1`/`0`, not every negative
signed 32-bit version. Send evidence records the **requested** version, not an
independently verified installed list. Accepted ACK, a later version query and
actual offline use are distinct facts.

Lost, malformed or unpaired replies do not acquire fabricated native evidence.
Reconnect/restart never replay an uncertain update or convert Differential to
Full. After Failed, VersionMismatch or uncertainty, reconcile explicitly and issue
a fresh authorized Full if resynchronization is intended; a version alone cannot
prove retained contents. `ClearCache` does not clear the list or service allowlist.

## Station-side OCPP 2.0.1 list and cache

The same independently default-off privileged demo controls also support exact
`ocpp201` roster stations. Provisioning routes the distinct `list201:` namespace
to a separately typed immutable provider. A mixed roster never attaches a 1.6
provider to a 201 station or accepts cross-edition command/result evidence.
Native values remain only in bounded protected startup memory and actual station
traffic, never service SQLite/WAL, results, history, diagnostics or public transports.
The 1.6 provider retains its historical version and Unicode behavior.

Native 201 primary, group and additional identifiers obey the OCPP identifierString
ASCII alphabet: letters, digits and `* - _ = : + | @ .`, at most 36 characters.
NoAuthorization requires an empty primary token; other types do not acquire an
invented nonempty rule. Identity compares the exact type and case-insensitive token,
not additionalInfo, and preserves original spelling. Valid personal messages,
languages, priorities, EVSE scope and nested vendor metadata stay private and inert;
these controls implement neither tariffs nor station display behavior.
Language tags follow bounded RFC5646 syntax; language2 requires a different language1.
Expiry timestamps have at most three fractional digits.

Send versions are positive i32. Full may replace a higher positive installed version;
Differential versions must advance at the station. Omitted Full clears installed
contents while retaining the submitted positive version; omitted Differential
changes only the version. Explicit empty arrays violate the pinned schema and are
not rewritten into omissions. Every Full entry requires idTokenInfo; its absence
is a deletion only in Differential. Upsert/delete behavior is station-owned, and
an ACK does not create a service-side installed-list ledger. Query zero means no installed
list, disabled or uninitialized, never the 1.6 unsupported -1 sentinel.

The shared 128-capability/256-entry/64KiB-complete-CALL/1MiB-retained/2MiB-file caps
apply without widening. Only correlated current-generation explicit device-model
facts learn LocalAuthListCtrlr ItemsPerMessage, BytesPerMessage, Entries, Enabled,
Available and SupportsExpiryDateTime, without component/variable instances.
Entries Actual is a count, including zero; integer maxLimit is separate capacity.
Full entry counts and Differential unique upsert counts must fit known capacity.
Differential deletions do not consume capacity; all entries still count toward
ItemsPerMessage and complete-CALL BytesPerMessage limits. No final installed
cardinality is inferred from counts, updates or ACKs.
AuthCacheCtrlr Enabled is separate. Reconnect starts these facts unknown.
ClearCache affects only the station cache, never the local list or service allowlist.
Safe native Send statuses are Accepted/Failed/VersionMismatch; Clear is
Accepted/Rejected. statusInfo and customData are omitted from public evidence.
Lost, malformed and unpaired responses remain uncertain without automatic retries,
Full resynchronization or replay. Explicit fresh authorized commands remain possible.

