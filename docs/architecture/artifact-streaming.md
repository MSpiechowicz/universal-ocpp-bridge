# Bounded operational artifact streaming

`uob_provider_adapter::artifacts::ArtifactTransfers` provides streaming byte I/O for future firmware
and diagnostic provider workflows. It uses the composition root's `RuntimeResourceBudget`, an
explicit buffer size, maximum artifact size, and an absolute per-call timeout. No transport address,
authentication policy or firmware workflow is selected by this utility.

`copy` moves bytes between `AsyncRead` and `AsyncWrite` transports. On Unix, `receive` streams into
a private temporary file in a caller-selected spool directory, returning a `TemporaryArtifact` only
after EOF. `send` consumes that artifact and streams it to an `AsyncWrite` transport from offset zero.
The artifact exposes its measured byte count, never a partially completed public path.

For example, a provider with already authorized transports can receive and forward an artifact:

```rust,ignore
let transfers = ArtifactTransfers::new(shared_budget, TransferLimits {
    buffer_bytes: 64 * 1024,
    maximum_artifact_bytes: 256 * 1024 * 1024,
    timeout: Duration::from_secs(300),
})?;
let artifact = transfers.receive(download, spool_directory, receive_cancelled).await?;
let transferred_bytes = transfers.send(artifact, upload, send_cancelled).await?;
```

Each call has its own deadline. The owning workflow must impose any deadline spanning multiple calls.
Cancellation is a caller-supplied future resolving to `()`, so workflow shutdown can interrupt reads,
writes, flushes, and file-operation waits. A caller may also drop the transfer future. Owned peer
transports are then dropped; when passing borrowed transports the caller remains responsible for
closing them and handling partial transmission. The utility never retries a partial operation.

## Memory, disk and backpressure

Each active transfer reserves one existing `MultipartAssembly` slot and exactly `buffer_bytes`
of the shared background byte allowance before allocating its single buffer. Sharing these slots
with multipart reports conservatively bounds total large background operations. Charger and
critical-report reserved capacity remains protected. Capacity exhaustion rejects immediately;
there is no internal waiting queue. Buffer sizes must be 1–262144 bytes; artifact size and timeout
must be nonzero.

The source is not read again until the previous chunk is written. Total measured bytes are checked
with overflow protection before writing each chunk; an oversized chunk is refused in full. EOF at
the exact maximum is accepted. Success requires destination flush, and records only completed byte
transfer, not charger acceptance, firmware installation, artifact authenticity, or durable storage.
HTTP/TLS/transport buffers and kernel caches are outside this utility's single-buffer accounting and
must have their own bounds. No whole-file payload is assembled in daemon memory.

The spool directory must be on a trusted, appropriately disk-budgeted filesystem, not an unbounded
memory-backed temporary filesystem. Each transfer is capped by `maximum_artifact_bytes`; callers
must also bound retained completed artifacts and aggregate disk usage. Completed artifacts do not
hold an in-memory transfer reservation.

Temporary files are created exclusively with mode 0600 and unpredictable names, then immediately
unlinked before peer bytes are read. The returned object owns the sole file handle; consuming or
dropping it releases the temporary storage. It is ephemeral staging and intentionally provides no
persistent path, fsync guarantee, crash recovery, or installation API. A persistent artifact provider
must separately implement its integrity, persistence, retention and publication policy.

File creation, reads, writes and seek run on Tokio's blocking pool, one operation at a time per
transfer. Each file job owns its handle, buffer and reservation. A cancelled transfer does not wait
for an already-started kernel operation, which cannot be forcibly aborted. That operation keeps its
capacity charged until it finishes, then drops all resources even if nobody awaits its result.
No subsequent chunk is scheduled. Permanently stuck filesystem I/O can therefore retain a bounded
slot and delay runtime shutdown; cancellation is not a promise to interrupt kernel I/O.

## Verification

```text
cargo test --locked -p uob-provider-adapter
./scripts/verify-workspace.sh
```

The suite streams a generated 129 MiB artifact (larger than the planned 128 MiB daemon RSS target)
through both direct transport and a real temporary-file round trip while checking a 64 KiB buffer
reservation and exact byte order. It also checks size limits, backpressure/read-ahead, critical
capacity, absolute deadlines, cancellation, output failures and temporary-resource cleanup. A
current-thread runtime test leaves a real TCP peer stalled while another socket exchange and
charger admission complete. A controlled blocking job proves cancellation retains accounting until
the job releases its file. These are utility/resource-isolation tests, not measured Pi RSS or
firmware-installation qualification. Device workflows remain separate backlog items.
