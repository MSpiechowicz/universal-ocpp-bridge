# Reversible release compatibility

Normal promotion is available only when the candidate and previous-good artifacts can both read
and write the exact public-contract, configuration, operational SQLite, and external-database
versions that will exist after promotion. Each immutable artifact publishes inclusive read and
write ranges for all four surfaces. A compatible release expands schemas before use and postpones
contraction until the artifact is outside the rollback support window.

The release manager consumes evidence from a real `old -> new -> old` qualification cycle. The
candidate must create transactions, exact meter data, command IDs, pending target deliveries,
export checkpoints, and audit records. The restarted previous binary must read those records and
successfully write further state. New enum or record values must round-trip without being dropped,
reinterpreted, or replaced with defaults. Fixture setup alone cannot claim that the candidate
created the records.

Configuration downgrade is a lossless view, not a state rollback. A projection must retain unknown
fields and must not modify either database. Artifact rollback reuses the upgraded operational
SQLite database and, when export is configured, the same external database; it never restores a
snapshot and never runs a reverse migration. Backups remain disaster-recovery inputs outside this
normal promotion path.

Both local and external migrations must be declared additive. An unsupported schema, lossy
projection, missing record class, destructive/reverse migration, substituted database, stale
artifact digest, or failed old-binary read/write blocks normal promotion. Such a candidate requires
a separately reviewed maintenance/recovery procedure.

Signed policy is evaluated independently of semantic-version ordering. The candidate and
previous-good artifact must be at or above the signed compatibility/security floor, must not be
revoked, and must match the exact digests named by the qualification evidence.
