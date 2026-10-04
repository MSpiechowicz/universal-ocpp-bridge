# Native OCPP 2.0.1 local authorization software peer

`uob-sim` owns a separately typed OCPP 2.0.1 station model. It does not import the
bridge's service/application authorization normalizers and does not translate an
OCPP 1.6 list, transaction or status into a 2.0.1 result. The existing 1.6 model and
wire conventions remain separate.

## Private station configuration

Use `bins/uob-sim/examples/local-authorization-2.0.1-config.toml` and
`bins/uob-sim/examples/local-authorization-2.0.1.toml` as synthetic examples.
Copy them into a **new**, owner-only directory, make the configuration, station
credential and scenario files mode `0600`, and replace the absolute private paths
and loopback endpoint. `[stations.local_authorization].private_state_file` must
have an owner-only canonical mode `0700` parent. It is not a service state path,
credential file or public export. Run:

```sh
cargo run --locked -p uob-sim -- run --config /absolute/private/config.toml \
  --scenario /absolute/private/scenario.toml --format jsonl
```

The station credential is a separately provisioned file. The demo's central
allowlist and privileged service update references are separate authorizations;
installing a local list does not grant central policy access. The scenario waits
for a real CSMS Full update before going offline. It is not a canned reply peer.
CLI JSONL is emitted when the run finishes, not as a live progress stream.
The joint harness uses its independent read principal for station snapshots and
all event-backed command status/history reads, including recovered results.
Only command submission uses the privileged control principal. Possession of a
submission credential is not evidence of read authorization.

Charging `expect_response` comparisons are whole-response equality, not partial
status matching. Fixed-clock process peers assert the complete independently
authored Boot response, including currentTime and interval. The live joint smoke
does not pin a daemon clock or compare a status-only object to a complete Boot
response: its relay plus station-registration gate observes a fresh current-socket
Accepted Boot. Native safe-counter assertions intentionally select named counters.
The joint variant adds an actual central Authorize before the Full gate and thus
expects one cached entry there; the standalone example has not populated that cache.

The state binds format, exact station and protocol `ocpp2.0.1`, checks file
ownership/mode/inode aliases and holds an exclusive writer lock. Atomic file
replacement and directory synchronization precede Accepted. Failed writes do not
partially change the in-memory list; uncertain post-replacement synchronization
makes the model unavailable until successful explicit recovery. Cross-edition,
corrupt and insecure files fail rather than silently resetting.

Limits are 256 list entries, 256 cached entries, 256 update entries, 128 queued
offline records, 64 KiB complete incoming CALL frames and 1 MiB retained private
state. Native GetVariables supplies bounded Actual values for LocalAuthListCtrlr
and AuthCacheCtrlr. `Entries` Actual is the installed count, not capacity.
GetBaseReport FullInventory and GetReport emit an actual correlated native
NotifyReport CALL after their Accepted ACK, with a separate Entries
`variableCharacteristics.maxLimit` of 256. Counts change when the list changes;
capacity does not. Root component/variable metadata names use allocation-free Unicode
default case folding, including long-s aliases such as `LocalAuthLiſtCtrlr` and `Entrieſ`;
instance/EVSE selectors never alias the root. Reports contain only seven bounded
read-only controller facts, never identifiers or private list/cache metadata.
SummaryInventory returns the supported Enabled/Available facts.
ConfigurationInventory returns EmptyResultSet because no variable is remotely
writable. GetReport Available/Enabled criteria use the controller's supported
enable/availability facts; Active/Problem criteria return NotSupported rather
than inventing unimplemented monitoring. Selectors are bounded to 256, and pending
NotifyReport acknowledgements to 128 per socket with the request deadline.
Unacknowledged reports are not retried or carried to another socket generation.
The shared configuration's `list_supported` controls native list availability and
enabled behavior; `cache_supported` controls cache enabled behavior. Both default
to true only for an explicitly configured durable model. A disabled retained list
returns version zero and does not suppress an enabled cache.

## Native semantics and privacy

- Update versions are positive `i32`; Differential versions at or below installed
  return VersionMismatch. Full can replace a list with a lower positive version.
- An omitted entire Full list clears entries and retains the submitted positive
  version. An omitted Differential list retains entries and advances the version.
  Explicit `[]` is invalid under the pinned JSON schema and is never rewritten.
- Each Full entry requires `idTokenInfo` under Part 2 AuthorizationData, even though
  the JSON property is optional. Differential information present means upsert;
  absent means typed-identity deletion. Duplicate typed identities fail atomically.
- Identity is native token type plus ASCII-case-insensitive identifier, preserving
  original spelling. The native identifierString primitive permits only ASCII
  letters/digits and `* - _ = : + | @ .`, with length zero through 36, including
  additional/group identifiers. NoAuthorization requires an empty token. Generic
  additionalInfo.type, personal messages and private extension content stay UTF-8.
  These restrictions do not change OCPP 1.6 Unicode identity behavior.
- An enabled list entry has priority even when blocked, expired or outside EVSE
  scope. It does not fall through to cached acceptance. Native C10 nevertheless
  refreshes the separate cache with the latest actual AuthorizeResponse or
  TransactionEventResponse information, regardless of status or list membership.
  Deletion reveals that latest cached information, including a denial. A full
  cache accepts a new identity by evicting an older received entry. Explicit cache
  expiry removes acceptance independently of list expiry support. Optional
  AuthCacheCtrlr.LifeTime is unsupported (UnknownVariable), not an invented default
  TTL. ClearCache only clears AuthCache and returns Rejected when disabled or when
  durable clearing fails.
- Native authorization statuses, expiry, EVSE scope, priority `-9..9`, BCP47
  languages, group/additional tokens, customData and personalMessage are preserved
  privately. Date-time fractional precision is at most three digits. Language2
  requires a different language1. Message and priority metadata is inert: it does
  not execute content or alter tariffs, scheduling or charging policy.
  Every authorization-metadata EVSE ID is a positive `i32`; zero/negative IDs
  reject the entire Full/Differential update and cannot refresh the central cache.

Without a durable model, GetLocalListVersion returns zero, SendLocalList yields
native CALLERROR NotSupported, and ClearCache returns Rejected. Never emit a fake
SendLocalList response status NotSupported or borrow 1.6's query version `-1`.
Safe scenario observations contain counters, availability or status only, not raw
tokens, group/additional identifiers, extension/message data or detailed native
statusInfo. Raw protected material belongs only in private synthetic inputs,
station-owned private state and the native OCPP wire.

## Offline facts, recovery and uncertainty

`csms_offline` actually closes the socket. `offline_start` and `offline_stop`
record valid original Started/Ended TransactionEvent payloads with typed token,
native transactionId, configured EVSE/connector, sequence and timestamp. Local
status, expiry, type, EVSE scope and resource occupancy gate a new transaction.
No native transaction ID is invented from a 1.6 numeric response.

Explicit `csms_reconnect`, ordinary automatic socket reconnection, and native Reset
recovery perform a real Boot on the new socket. Only its correlated Accepted
response permits original persisted offline delivery. Native Reset first ACKs,
then closes and recovers from disk; it is not equivalent to CLI process death.
Delivery retains original timestamp spelling and payload facts instead of allowing
an SDK date serializer to change them. Sending is persisted before transmission;
a killed process recovers Sending as Uncertain. Uncertain records stay bounded,
block automatic replay, and are not reinterpreted as certainly undelivered.
CSMS SendLocalList/ClearCache commands are never automatically replayed or retried.
Delayed/lost replies occur after durable commit, not by faking an Accepted result.

## Actual daemon plus independent simulator smoke

The opt-in standard-library harness starts **actual separate** `uob serve` and
`uob-sim run` processes, creates fresh synthetic loopback credentials and private
configuration, exercises authenticated privileged commands, and checks native
results/history and SQLite transaction evidence. It never reads live credentials.
Build the binaries using the project's pinned toolchain, then run:

```sh
cargo build --locked -p uob-service -p uob-sim
python3 bins/uob-sim/tests/native201_joint_smoke.py \
  --bridge "$PWD/target/debug/uob" \
  --simulator "$PWD/target/debug/uob-sim" \
  --output /absolute/new-private-native201-evidence
```

The output directory must not exist. The harness provisions an opaque Local token
independently in the central policy, uses distinct read/privileged API grants,
installs Full, exercises offline type/EVSE denial and original transaction delivery,
then checks real populated-cache clearing while preserving exact separate list
contents and central authority. Actual positive and nonaccepted Authorize responses
repopulate the cache. Subsequent phases observe a real delayed mutating ACK, a
dropped ACK after private mutation, genuine simulator process kill/reopen with
retained list/cache, and actual daemon restart with preserved historical uncertainty.
A transparent loopback TCP observer forwards original bytes and counts native
commands/ACK timing; it never generates a handshake, CALL or reply. Fresh query
after restart must not cause any old native mutation to reappear. Final deletion,
stale Diff, lower omitted Full and positive empty-list query exercise native version
semantics. Original transaction ID/EVSE/sequence/timestamps are verified in actual
service SQLite, and DB/WAL plus every process output are scanned for list/cache,
central-policy, credential and grant markers. Private `smoke.json` is written only
after these checks succeed, with binary hashes and payload-free wire observations.
Cleanup stops only the harness's processes; a nonzero exit is never positive evidence.

Native model, cache, compiled-process and corpus verification commands:

```sh
cargo test --locked -p uob-sim --test local_authorization201_model
cargo test --locked -p uob-sim --test local_authorization201_cache
cargo test --locked -p uob-sim --test local_authorization201_process
cargo test --locked -p uob-sim --test local_authorization201_faults
cargo test --locked -p uob-ocpp-fixtures
cargo run --locked -p uob-ocpp-fixtures
```

The process tests use the compiled independent simulator binary against a real
WebSocket peer: killed-process recovery, denied Boot/no delivery, lost transaction
ACK/uncertain recovery, delayed ACK/deduplication and dropped management ACK/no
mutation replay, actual GetBaseReport/GetReport and NotifyReport count/capacity
distinction, and caseless controller identities. The joint harness was executed
successfully against separate compiled daemon and simulator processes, including
delayed/lost ACKs, private-state recovery, original-fact delivery and no mutation
replay. Passing software-boundary checks is not physical charging, hardware
qualification, OCA certification, universal exactly-once delivery or third-party
memory-erasure assurance.
