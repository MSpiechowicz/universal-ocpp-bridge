# Durable command deduplication

Every authorized command ingress uses the application-owned `OperationalStore` before a charger
action can be dispatched. The SQLite adapter performs command admission inside the same immediate
transaction as the command's journal, outbox, and export mutations. Only an `Admitted` outcome may
cause a new charger action; a `Duplicate` outcome returns the latest durable result when one is
known and applies none of the bundled mutations again.

## Stable identity and conflicts

The caller's request ID is the lookup key. The stored content fingerprint is SHA-256 over a
canonical JSON representation of the command after recursively sorting object keys and excluding
the request ID and host-assigned admission time. The fingerprint still covers the contract
version, correlation ID, charging resource, operation and parameters, expiry, and authenticated
origin. A retry may therefore receive a new host admission time without becoming a different
command, while changing its station, operation, parameters, expiry, or authenticated route is a
conflict.

A conflicting reuse returns the stable `StorageErrorCode::Conflict` category with sanitized static
detail. Storage errors do not expose the submitted payload, credentials, database statements, or a
driver error chain.

## Results and retention

The latest `CommandResult` remains keyed by the same request ID. Identical retries receive that
result without dispatch. Each command identity has a retention boundary seven days after its
original durable admission. The host calls `prune_command_deduplication` with trusted UTC time;
the prune transaction removes an eligible command and result together.

Only commands with a resolved lifecycle can expire. Commands with no result, `Admitted`,
`Dispatched`, or `TransmissionUncertain` state remain protected regardless of age. In particular,
an uncertain non-idempotent operation is never made dispatchable again merely because seven days
elapsed.

Schema version 3 adds the fingerprint, admission instant, retention boundary, and unresolved flag.
Rows created by the earlier operational-store schema are decoded and backfilled inside the first
prune transaction, including their latest known lifecycle, before any retention decision is made.
