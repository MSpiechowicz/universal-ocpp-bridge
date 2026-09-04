# OCPP 1.6J authorization

OCPP `Authorize` and `StartTransaction` identifiers are validated with the pinned 1.6J model, held
as sensitive token bytes only for provider resolution, and decided by the application-owned local
authorization service. Protocol code receives only the resulting opaque reference or safe denial
reason. Raw `idTag` values are not retained in events, diagnostics, SQLite, or response errors.

`Authorize.conf` preserves the request message ID and maps active, revoked, expired, unknown,
out-of-scope, and provider-unavailable decisions to the native `Accepted`, `Blocked`, `Expired`, or
`Invalid` statuses. A trusted allowlist expiry is returned for an accepted identity so a station can
invalidate its authorization cache at the correct instant.

Every authorization-bearing call consults the current durable policy. A prior successful
`Authorize` result is not a bridge-side positive cache: revocation before `StartTransaction` denies
the start, and the same denial remains effective after service recovery. The transaction path also
requires the trusted canonical resource to match the native connector in the call. Provider delays
remain pending instead of producing a premature success, and malformed identifiers fail before
policy evaluation.
