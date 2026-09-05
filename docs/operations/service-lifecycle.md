# Service packaging and shutdown

`packaging/systemd/uob.service` runs the current `uob serve` executable as the dedicated
`uob:uob` system account. The unit starts the management service without static UI assets.
It does not enable the separately packaged simulator or the planned release supervisor.
The current CLI validates target selection but does not yet compose a charging runtime;
external database export remains unavailable. Installing this unit does not change those
implementation boundaries.

## Installation

Use a Linux host with systemd 245 or newer (journal namespaces are required). Install a
verified native `uob` executable for the host architecture at `/usr/local/bin/uob`, owned
by root and executable by the service account. From the repository root, as an administrator:

```sh
install -m 0644 packaging/systemd/uob-sysusers.conf /etc/sysusers.d/uob.conf
systemd-sysusers /etc/sysusers.d/uob.conf
install -d -m 0750 -o root -g uob /etc/uob
install -m 0640 -o root -g uob packaging/systemd/bridge.toml /etc/uob/bridge.toml
install -m 0644 packaging/systemd/uob.service /etc/systemd/system/uob.service
install -m 0644 packaging/systemd/journald-uob.conf /etc/systemd/journald@uob.conf
```

Edit `/etc/uob/bridge.toml` with the installation's unique bridge identity before starting.
Keep credentials root-owned, readable only by the service group, and outside logs. On an
existing installation, preserve its configuration instead of replacing it with the example.
Then verify and start:

```sh
systemd-analyze verify /etc/systemd/system/uob.service
systemctl daemon-reload
systemctl enable --now uob.service
systemctl show uob.service -p User -p Group -p MainPID
ps -o user,group,pid,args -p "$(systemctl show uob.service -p MainPID --value)"
journalctl --namespace=uob -u uob.service
```

The service gets `/var/lib/uob` and `/run/uob`, both mode 0700, with a 0077 umask.
Configuration and executables are read-only under the unit's filesystem protections.
No runtime root privileges or capabilities are granted. Management stays on loopback.

The separate journal namespace limits persistent logs to 32 MiB, individual files to
4 MiB, volatile logs to 8 MiB, and retention to seven days. Journal rotation can temporarily
retain an active file beyond the budget; these are journald retention limits, not a disk quota.
Rate limits bound bursts, and forwarding is disabled so this package does not create an
unbounded second log sink. Restart `systemd-journald@uob.service` after changing its policy.

## Shutdown contract

SIGTERM and SIGINT stop management ingress and allow active requests to drain. Configure
`[lifecycle].shutdown_timeout_seconds` from 1 through 300; the default is 20 seconds.
`uob config check` rejects invalid values before startup. Deadline expiry produces exit code 1;
a completed drain exits 0. Runtime teardown gets one additional second for blocking library
work. Set systemd `TimeoutStopSec` above the application deadline plus this teardown margin
(default 25 seconds). Change the unit through a systemd drop-in when changing the deadline.
`KillMode=control-group` and `SendSIGKILL=yes` provide the final bound for a non-cooperative
thread or process; no watchdog or rollback policy is introduced here.

Protocol station/call and target handles retain ownership during `wait` and `shutdown`.
Cancelling those futures aborts their worker instead of detaching it. Target delivery workers
have a 20-second default shutdown bound and an explicit `shutdown_with_deadline` override.
On normal timeout they abort and join before reporting the missed deadline. MQTT shutdown
also joins its protocol, command, and report tasks. EMS listener ownership closes event
streams and cancels the listener when its parent is cancelled. Management event response
bodies own their producer tasks, so dropped responses cancel pending source work.

Hosts composing storage must stop producers and subscribers first, shut down target/delivery
workers within the remaining shared deadline, and finally call `SqliteOperationalStore::shutdown`.
That operation closes admission across **all clones**, drains already accepted operations,
and joins the dedicated SQLite thread. It does not cancel a transaction mid-commit. A timeout
retains thread ownership and permits a later call to finish joining; it does not claim the
store is closed. Stop the process under the supervisor's final bound if the thread remains
stalled. Cancellation of a caller's write future does not undo accepted database work.

On restart, recover from the existing SQLite database and WAL. Preserve committed events,
commands, pending deliveries and uncertain outcomes; do not delete WAL files, restore an older
database, mark deliveries acknowledged, or blindly replay charger controls because shutdown
expired. SQLite atomic transaction recovery decides the fate of any unacknowledged write.

## Verification

`cargo test --locked -p uob-service -p uob-storage-adapter -p uob-target-adapter` covers
repeated real-process SIGTERM, listener release, offline deadline validation, cancellation of
supervisor futures, joined deadline cancellation, stalled storage ownership, and durable
outbox preservation after draining accepted writes. The workspace suite also runs the MQTT
wire reconnect/shutdown and EMS event-stream tests. Real external-export reconnect testing
belongs to the future export scheduler; disabled export creates no tasks or connections.
The package policy test checks the dedicated identity and retention settings; installation
and the `ps` check above verify actual non-root execution on a systemd host.
