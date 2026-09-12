# Offline capture inspector

Choose **Offline capture inspector** in the console navigation, or open `/?offline=1` on the
management console. This loads a separate view without mounting the live application. Once the
static console assets are loaded, file inspection works with network access disabled. It does not
install a service worker or promise that a fresh page load works without those assets.

Select an explicitly downloaded version 1.0 JSONL capture. The inspector reads the local file;
it does not upload it, request service identity, open a stream, start capture, or contact a charger,
target, or export provider. It has no credentials, live commands, or replay controls. Following the
explicit **Leave offline inspector and connect to a service** link opens the live console afresh.

The persistent **OFFLINE FILE** badge distinguishes file inspection from live production, staging,
or demo access. The provenance area shows the environment recorded in the file, bridge/process,
release/digest, build, capture ID, station/target filters and capture level. These are unverified
claims from the file, not authenticated identity or a signature verification result.

## Validation and bounds

Import requires one manifest, zero to 2,000 trace lines and one terminal summary, in that order,
with a final newline. The checked-in capture, service-identity and trace-record schemas are bundled
locally without prose annotations. No schema references are fetched. The generator fails if a new
schema keyword needs validator support; the frontend build checks for schema drift. Regenerate with
`node frontend/scripts/capture-schema.mjs` after an intentional source-schema change.

Before reading, the importer enforces a **9 MiB** file ceiling. It reads at most **32 KiB** per
slice, buffers at most **64 KiB plus 32 bytes** for a JSONL wrapper, limits manifest/summary lines
to **16 KiB**, and rejects nesting deeper than **32** before parsing JSON. Canonical records retain
the existing **64 KiB** ceiling. Declared byte and record limits cannot raise these local limits.
Only capture schema **1.0** and trace schema **1.0** are accepted. Unsafe integer counters,
invalid UTF-8, malformed or unsupported schemas, duplicate/out-of-order sequences, wrong process
identity, records outside the initial cutoff, missing metadata, and inconsistent summary counts
are rejected. A failed or canceled import clears its partial buffer and the previous capture.

The timeline reuses the live inspector's **4 MiB / 2,000-row** display ring, ten mounted rows,
**256 KiB** detail cache, inert JSON formatting, and 64 temporary bookmarks. A valid file larger
than the display ring opens with explicit browser evictions; counts in the file provenance still
refer to the full validated file. Encoded byte limits are not measurements of total browser heap.

Search and filters, message/state inspection, and linked command evidence operate only on retained
file rows. Imported correlation IDs never trigger a live lookup. Source strings and unknown fields
remain inert text. HTML-like content is not executed, linked, or interpreted as instructions.

## Gaps and lifetime

The view preserves the initial and last observed windows, prior evictions, drops, detail shedding,
missing sequences, unexported initial records, and truncated records. It shows export termination
separately from window completeness. A valid interrupted export with a consistent summary can be
inspected as incomplete; a missing/partial summary is rejected. Even a complete retained window
never means complete history or current live state.

File data, filters, and bookmarks stay in tab memory. Nothing is automatically written to
localStorage, sessionStorage, IndexedDB, or a server. **Clear offline capture** cancels an active
import and clears its data. Replacing the file also clears the previous capture. A reload requires
selecting a file again. No automatic replay or imported-data download is offered.

## Verification

Frontend unit tests cover actual schema validation, summary and sequence consistency, unsupported
versions, byte/line/record/depth ceilings, UTF-8 crossing slice boundaries, cancellation, partial
exports and bounded display eviction. Browser tests import a file from the real Rust export route
with network disabled, retain provenance/correlation evidence, exercise hostile content, filters,
bookmarks, mobile layout, replacement and cancellation, and verify that no API requests occur.
