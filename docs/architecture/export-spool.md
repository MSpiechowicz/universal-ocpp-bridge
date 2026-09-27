# Local external-export spool (#99)

The optional, offline `ExportIngestor` reads **committed** records from the authoritative
operational SQLite store and copies them into a separate `SqliteExportSpool`. This is a local
source-to-spool boundary, not a PostgreSQL delivery worker. `uob serve` still rejects
`data_export.enabled = true`; there is no production export scheduler, remote acknowledgement,
or spool-drain/delete API. A spool checkpoint is **not** proof of remote delivery.

The spool's SQLite implementation and `ExportSpoolLimits` live in
`uob-storage-adapter`; `uob-external-export-adapter` owns the external
provider catalog, offline registry and transport security validation.

## Source and recovery

Operational schema v12 assigns per-durability `source_sequence` positions to surviving committed
records, maintains separate critical and best-effort telemetry high-water marks, and stores a
source-generation identity. New sources start with a complete known baseline. Upgrading an
older operational database numbers only records that still exist, in old row order within each
stream. It cannot reconstruct rows deleted before migration: the durable
`legacy_baseline_incomplete` flag reports that unknown history, even if the older database now
contains no records. Do not present the v12 starting position as proof that nothing was lost
before migration.

The ingestor processes the streams separately, critical first, for up to 100
discovery/record steps per stream per call. Source discovery returns bounded
metadata (positions, field lengths and a source-row locator), not whole records.
For each admitted record, it reads the stored record ID, timestamp and JSON
payload in at most 64 KiB byte chunks, moving each budgeted chunk to the spool.
Pending enumeration likewise returns descriptors rather than decoded records;
consumers read each pending field in at most 64 KiB chunks. Neither the source
locator nor a pending descriptor is a remote-delivery acknowledgement. Normal
copying does not materialize a whole large record; the authoritative producer's
existing write path is not a streaming-write contract.

The ingestor durably observes discovered source high-water, legacy-baseline
state and destination binding before optional copying, including when the first
retained critical record cannot fit. Observation does **not** move its checkpoint.
A spool transfer allocates the pending row and writes its fields within one
spool-only transaction. The pending bytes, checkpoint, exact inclusive
`SourceExpired` gaps and any provisional telemetry evictions commit together;
failure, dropped transfer or crash before commit rolls them all back. After a
completed commit, reopening sees the entire record and checkpoint, not a
partial transfer. The source is not pinned during transfer: if retention removes
a record between chunk reads, the provisional copy is aborted, then discovery
from the unchanged checkpoint establishes any exact missing positions. An
expired operational record is never recreated by the spool.

A retained critical record that is individually too large for the *usable*
spool capacity, or cannot currently be admitted, causes backpressure without
copying it, advancing its record checkpoint or inventing a loss gap. Its
observed high-water and original destination/source ownership remain durable.
Telemetry still gets its own pass and checkpoint: it can be copied, recorded
as `TelemetryDropped`, or older pending telemetry can be evicted with exact
`TelemetryEvicted` gaps to admit critical data. If retention later removes the
deferred critical record, discovery can commit an exact `SourceExpired` interval
and sticky `incomplete` status, then continue with surviving records. Leading
positions already proven lost may advance independently before a still-retained
blocked record. A full bounded gap summary instead backpressures; no cursor
crosses unaccounted loss. Legacy-unknown, high-water, pending count and exact
gaps remain durable for inspection after restart. No status field implies remote
delivery.

Operational retention can accept an individual record larger than the spool's
usable capacity under its separate 256 MiB logical budget. Chunking removes
the former 1 MiB *read* barrier and allows records of several MiB to pass, but
does not make this fixed-size spool lossless across the full accepted range.
Such an over-budget critical record remains at its checkpoint while retained;
after genuine source expiry, only an exact loss gap permits progress. Increasing
the per-chunk limit or silently advancing over retained data cannot change that
physical constraint.

The single spool binding includes destination ID, immutable configuration revision, provider
kind and operational source generation. A mismatch is rejected rather than relabeling pending
records or checkpoints. The offline registry also rejects a destination/revision change or
export disablement while a backlog is pending, except for a separately authorized discard with
a durable audit event naming the original destination. No daemon management endpoint currently
performs that discard. Never erase the spool as an implicit reconfiguration or recovery step.

## Physical envelope and location

The default main SQLite file is capped at 60 MiB (4 KiB pages); DELETE-mode
rollback journaling can add under 62 MiB for a full rewrite, leaving 6 MiB of
the **separate 128 MiB spool physical main/rollback envelope** for metadata and
block rounding. This is not 128 MiB of payload (nor a guaranteed 60 MiB payload
allowance): table/index pages, free pages, rollback headroom, a protected
page reserve and 64 KiB allowance reduce usable capacity. Admission checks
the three field lengths, then physical page use; it can backpressure before
the logical ceiling. `synchronous=FULL`, in-memory SQLite temporary storage,
no WAL, a bounded worker queue and a maximum of 512 gap intervals keep spool
work separate from the operational SQLite worker and its charging reserve.
Limits for tests can only be made smaller. This envelope is not an automatic
kernel quota: provision and verify an independent hard-capacity, durable,
physically isolated location with enough capacity for filesystem metadata and
the full 128 MiB working envelope. A same-partition directory, bind mount,
tmpfs, or loop image does not establish a production durable physical budget.
Operational SQLite, including its WAL, must retain its own reserved capacity
when the spool reaches ENOSPC.

Spool schema v2 stores pending records in a rowid table with three incrementally
written/read BLOB fields. Opening a v1 spool migrates its bounded (at most
1 MiB per legacy envelope) pending rows one at a time in a single transaction:
the original destination binding, checkpoints, high-water, gaps and pending
order survive. Migration uses bounded memory and reuses pages as old rows are
replaced, rather than staging a duplicate database. A failed or interrupted
migration rolls back to recoverable v1; the schema switches to v2 only at
commit. Near-full v1 layouts containing both large and many small rows have
been exercised under the actual 60 MiB main-file ceiling. Do not discard or
reset a spool to work around migration or capacity pressure.

`SqliteExportSpool::open` requires absolute paths, an existing canonical mode-0700 private spool
directory on a different device from both the operational database's parent (where WAL files
reside) and any existing operational database file target, and private mode-0600 spool files.
It rejects unexpected sidecars and linked spool files on open. These checks reject a shared
device but **do not** prove the mount's physical backing, hard quota, persistence, or
allocation independence; deployment must prove them separately. Do not use the earlier
`/var/lib/uob/export-spool/` or `/var/lib/uob-staging/export-spool/` reserved directories on
the corresponding operational partitions. Production export remains unavailable, so no new
production mount or directory is required by the shipped service.

`python3 -B scripts/test-export-spool-isolation.py --disposable` is an opt-in **privileged
non-production** fault probe. It creates two preallocated disposable ext4 loop images in a
private mount namespace, fills only the spool filesystem through fsynced writes to ENOSPC,
checks unchanged operational free blocks and an additional fsynced operational write, then
unmounts and removes both images. The loop images test kernel allocation isolation; they are
not an accepted production topology. The script does not invoke Rust ingestion, demonstrate
spool checkpoint recovery, or prove service-level OCPP isolation. Those paths require their
separate Rust and real-service acceptance exercises.
