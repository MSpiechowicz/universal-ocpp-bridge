# Isolated Compose target demos (#94)

The opt-in launcher exercises the real `uob` daemon, independent `uob-sim` OCPP 1.6J and 2.0.1 charging peers, and a separate target-side client. Run it from the repository root with a working Docker daemon, Docker Compose v2, `openssl`, `jq`, `timeout`, and `sha256sum`; provisioning also uses `date`, `find`, and the pinned helper images. Allow Docker access, image pulls, disk space and time for a local locked Rust release build (the first build can be slow). No local Rust or Node installation is required. The launcher creates a fresh, private `target/compose-demo.<random>` directory and a unique `uob94-<random>` Compose project per invocation. Do not use production credentials or data.

## Run the three targets

```sh
./scripts/compose-demo.sh run mqtt
./scripts/compose-demo.sh headless ems-http
./scripts/compose-demo.sh headless ems-mqtt
./scripts/test-compose-profiles.sh
```

`run` and `headless` both run the same non-browser verification and exit; `headless` explicitly names the intended API-only mode. The daemon is started with `--no-ui` in both. The acceptance runner executes **all three** target selections through the public `headless` launcher, checks independent OCPP 1.6J/2.0.1 client evidence and the simulator's final `run_passed` JSONL summary, verifies the owned project and run directory have gone, and exits nonzero on a failure or timeout. Its per-profile whole-launch timeout defaults to 1800 seconds; override with `DEMO_PROFILE_TIMEOUT_SECONDS=3600 ./scripts/test-compose-profiles.sh` for a slower build host. This is an opt-in integration run, not a source-text/configuration assertion.

| Mode | Selected target | Target-side peer | Broker |
| --- | --- | --- | --- |
| `mqtt` | Generic MQTT (`kind = "mqtt"`, standard profile) | MQTT 3.1.1/TLS client | Private Mosquitto TLS broker |
| `ems-http` | Direct EMS/SCADA HTTP (`kind = "ems-scada.http"`) | HTTP reads/commands client | **Absent**: neither started nor required |
| `ems-mqtt` | MQTT EMS/SCADA preset (`kind = "mqtt"`, EMS profile) | EMS MQTT/TLS client | Private Mosquitto TLS broker |

Expect a `Compose project:` line and a `Run directory:` line (never a secret), charging-verification lines for both OCPP editions, and a sanitized simulator `{"event":"run_passed","status":"passed",...}` summary. The exact generated IDs and host ports vary. Any target-client or simulator failure is nonzero; `run`/`headless` shut down and remove **only their own** project and private directory on completion, failure or interruption. If Docker cleanup itself fails, the launcher returns nonzero and retains the directory for an explicit retry; do not blindly delete its state. A passing exit proves the selected adapter's local demo path, not production charger behavior or end-to-end export.

The simulator receives two different charger credentials, boots each station, reports native status, waits for a real remote start, sends the native transaction start and meter values, then waits for remote stop and sends the native end. For OCPP 1.6J, the accepted remote start authorizes its `idTag` for the simulator's StartTransaction; no separate Authorize CALL is sent. OCPP 2.0.1 sends an Authorize CALL and reports a tokenless TransactionEvent Started correlated to the accepted RequestStartTransaction. The target peer verifies charging and remote commands; the simulator's success report is separate evidence. OCPP 2.0.1 uses the `remoteStartId` from its **accepted awaited** RequestStartTransaction rather than a fixture constant. The 1.6 transaction ID in this *fresh* demo database is 1; do not apply that assertion to an existing station/database. See [scenario runner](../simulator/scenario-runner.md) for the explicit awaited-ID option.

## Browser on this host only

```sh
./scripts/compose-demo.sh browser mqtt
./scripts/compose-demo.sh browser ems-http
./scripts/compose-demo.sh browser ems-mqtt
```

Run one command in a terminal and leave it open. `browser` executes the same charging and target-peer checks, then keeps the daemon and gateway alive until Ctrl-C. Use the printed `Browser gateway: http://127.0.0.1:<random-port>/` on **that host**; the gateway is bound to host loopback only. Browser mode omits `--no-ui`, serves the compiled assets already embedded in the daemon and does not require an external UI server. It does **not** make charging, management, broker or EMS listener ports public. Ctrl-C stops the project and removes its private directory. The console needs the generated scoped `reader` bearer for reads, and separately the generated `operator` or `privileged` bearer for eligible commands. These are private files in the printed run directory under `runtime/secrets/`, owned for the non-root container UID (10001). Transfer a token into the local browser via a trusted local clipboard/password-manager mechanism; **do not** print it in the shell, put it on a command line or URL, commit it, paste it into logs, or confuse it with either station socket credential. For a Wayland desktop with `wl-copy` and appropriate permission to read the file, a local clipboard pipe avoids terminal output: `sudo cat "$RUN_DIRECTORY/runtime/secrets/reader" | wl-copy` (replace `reader` with `operator` for the control input). Clear the clipboard after use. The console retains entered credentials only in tab memory; closing/reloading the tab clears them. This plaintext HTTP entry point is allowed solely on local loopback for a disposable demo, not across a network or for production use. A successful remote-command admission or native acceptance is not proof of a physical charging effect.

For a detached API-only environment (no simulator/client run), use:

```sh
./scripts/compose-demo.sh up ems-http
./scripts/compose-demo.sh down "$RUN_DIRECTORY"
./scripts/compose-demo.sh cleanup "$RUN_DIRECTORY"
```

Set `RUN_DIRECTORY` to the **exact path printed by `up`** without displaying any secret file. `up mqtt` and `up ems-mqtt` also start their private broker. `down` stops only that run's Compose project and retains its private directory/state; `cleanup` stops it and removes only its private files (including generated secrets). Detached `up` is **not** an acceptance run and needs explicit `cleanup` when finished. Never use `docker compose down --volumes` against another project or bulk-delete `target/` to tidy these demos.

## Isolation and limits

All peers share the daemon container's **private, nonproduction network namespace** (`network_mode: service:daemon`) to use its loopback listeners. This is not a staging or production namespace and must not be attached to either. A new run has a separate random bridge/project identity, credential files, MQTT ACL/users, SQLite state, and generated one-day demo CA/server certificate. MQTT clients explicitly trust only that run's CA (`ca.crt`) and connect to `mqtts://localhost:8883`; do not install this CA as a machine/browser trust root. Direct EMS HTTP and charger WebSockets are plaintext **inside that shared namespace only**. The browser gateway is an exception: its random host port is **loopback-only** and speaks HTTP; do not forward or expose it remotely. Secrets are file-mounted rather than supplied as Compose environment variables or printed in successful output.

`uob`, `uob-sim`, and `uob-compose-client` are compiled together from the pinned Docker build but copied into **separate, non-root runtime images**; the daemon does not launch the simulator or target peer. Pinned Mosquitto and nginx images supply the private broker and optional browser gateway. Runtime containers are read-only with dropped capabilities, `no-new-privileges`, UID 10001, no restart, a 128 PID limit and one CPU each. Daemon memory is capped at 768 MiB; simulator/client at 512 MiB each; broker/gateway at 128 MiB each. `/tmp` is a 16 MiB tmpfs; broker data/log tmpfs are 4 MiB each. Per-container JSON Docker logs rotate at 2 MiB × 2 files. Builds, Docker image storage and bind-mounted SQLite/state still consume host disk; these runtime caps are **not** a host-wide resource quota. The launcher bounds daemon readiness (30 attempts), client execution (180 seconds) and simulator completion (120 seconds); the acceptance runner also bounds the *whole* build-and-run profile.

Limits: This demo has **no PostgreSQL service, external export, or export acceptance** (issue #95); do not infer export readiness from the local SQLite run. EMS MQTT's current demo target does **not** populate the full EMS point catalog, so lack of catalog entries is not evidence of complete point export. Browser access is local and opt-in; there is no production-grade TLS endpoint, separate staging namespace or external supervisor in this Compose demo. The target profiles demonstrate selected adapter paths, not all deployment, authorization, failover or production trust policies.
