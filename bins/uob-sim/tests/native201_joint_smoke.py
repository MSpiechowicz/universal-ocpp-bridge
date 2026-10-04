#!/usr/bin/env python3
"""Opt-in actual uob + independent uob-sim smoke. Synthetic loopback only."""
import argparse
from datetime import datetime, timedelta, timezone
import hashlib
import json
import os
from pathlib import Path
import secrets
import socket
import sqlite3
import subprocess
import time
import tomllib
import urllib.error
import urllib.request
from native201_joint_flow import authorize, future_expiry, run_strengthened, scenario_document
from native201_joint_transport import NativeWireRelay

ROOT = Path(__file__).resolve().parents[3]
MARKERS = [b"list-marker-112", b"group-marker-112", b"message-marker-112",
           b"additional-marker-112", b"custom-marker-112", b"info-marker-112",
           b"central-policy-112", b"synthetic-station-secret-112"]


def require(condition, label):
    if not condition:
        raise RuntimeError(label)


def private(path, content):
    with path.open("x", encoding="utf-8") as file:
        os.chmod(path, 0o600)
        file.write(content)
    return path


def port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def wait_for(predicate, label, seconds=10):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.05)
    raise RuntimeError(label)


class Smoke:
    def __init__(self, bridge, simulator, output):
        self.bridge, self.simulator, self.output = bridge, simulator, output
        self.children, self.logs = [], []
        self.management, self.charging = port(), port()
        while self.charging == self.management:
            self.charging = port()
        self.relay = NativeWireRelay(self.charging)
        while self.relay.port in [self.charging, self.management]:
            self.relay.close()
            self.relay = NativeWireRelay(self.charging)
        self.grants = {name: "uob1.demo." + secrets.token_hex(16) for name in ["read", "control", "privileged"]}
        self.markers = MARKERS + [grant.encode() for grant in self.grants.values()]
        for name, grant in self.grants.items():
            private(output / name, grant)
        private(output / "station-auth", "synthetic-station-secret-112")
        private(output / "start-token", "central-policy-112")
        self.references, self.updates = {}, []
        for name in ["full", "differential-absent", "differential-delete", "full-absent"]:
            payload = json.loads((ROOT / "tests/ocpp-fixtures/corpus/wire/2.0.1" / ("local-list-" + name + ".json")).read_text())[3]
            for entry in payload.get("localAuthorizationList", []):
                entry["idToken"]["type"] = "Local"
                entry["idToken"]["idToken"] += "-x"
            self.add_update(name, payload)
        self.add_update("delay-diff", {"versionNumber": 20, "updateType": "Differential"})
        self.add_update("drop-diff", {"versionNumber": 21, "updateType": "Differential"})
        deletion = json.loads(json.dumps(next(entry["request"] for entry in self.updates if entry["request"]["versionNumber"] == 9)))
        deletion["versionNumber"] = 22
        self.add_update("differential-delete-after-restart", deletion)
        private(output / "updates.json", json.dumps({"updates": self.updates}))
        state = output / "service-state"
        state.mkdir(mode=0o700)
        private(output / "bridge.toml", f"""[bridge]
id='joint-112'
environment='demo'
[management]
listen_addr='127.0.0.1:{self.management}'
[charging]
enabled=true
listen_addr='127.0.0.1:{self.charging}'
state_directory='{state}'
read_grant_file='{output / 'read'}'
control_grant_file='{output / 'control'}'
privileged_grant_file='{output / 'privileged'}'
local_authorization_updates_file='{output / 'updates.json'}'
[[charging.stations]]
id='alpha'
protocol='ocpp201'
credential_file='{output / 'station-auth'}'
start_token_file='{output / 'start-token'}'
get_local_list_version=true
send_local_list=true
clear_cache=true
[[charging.stations.resources]]
evse_id='one'
native_evse_id=1
connector_id='one'
native_connector_id=1
[[charging.stations.resources]]
evse_id='two'
native_evse_id=2
connector_id='one'
native_connector_id=1
""")
        private(output / "sim.toml", f"""schema_version=1
[[stations]]
id='alpha'
endpoint='ws://127.0.0.1:{self.relay.port}/ocpp/alpha'
ocpp_version='2.0.1'
credentials_file='{output / 'station-auth'}'
request_timeout_ms=2000
reconnect=false
[[stations.evses]]
id=1
connectors=[1]
[[stations.evses]]
id=2
connectors=[1]
[stations.local_authorization]
private_state_file='{output / 'sim-state.json'}'
""")
        base = datetime.now(timezone.utc).replace(microsecond=123000) - timedelta(seconds=4)
        self.started_at = base.isoformat(timespec="milliseconds").replace("+00:00", "Z")
        self.ended_at = (base + timedelta(seconds=1)).isoformat(timespec="milliseconds").replace("+00:00", "Z")
        scenario = (ROOT / "bins/uob-sim/examples/local-authorization-2.0.1.toml").read_text()
        scenario = scenario.replace('type = "Local"', 'type = "KeyCode"').replace('type = "Central"', 'type = "Local"')
        scenario = scenario.replace('"list-marker-112"', '"list-marker-112-x"')
        scenario = scenario.replace("2026-10-04T00:00:01.123Z", self.started_at).replace("2026-10-04T00:00:02.123Z", self.ended_at)
        steps = tomllib.loads(scenario)["steps"]
        steps.insert(2, authorize("central-cache-population", "central-policy-112", "Accepted"))
        # This live variant populates one cache entry before its Full-install gate.
        next(entry for entry in steps if entry["id"] == "await-full")["expect_response"]["cacheEntries"] = 1
        private(output / "scenario.toml", scenario_document(steps))
        self.evidence = {"checks": [], "executables": {
            "bridge_sha256": hashlib.sha256(bridge.read_bytes()).hexdigest(),
            "simulator_sha256": hashlib.sha256(simulator.read_bytes()).hexdigest()}}

    def add_update(self, name, payload):
        reference = "list201:" + secrets.token_hex(32)
        self.references[name] = reference
        self.updates.append({"station_id": "alpha", "update_reference": reference,
                             "expires_at": "2099-01-01T00:00:00Z", "request": payload})

    def start(self, binary, arguments, label):
        stdout = (self.output / (label + ".stdout")).open("xb")
        stderr = (self.output / (label + ".stderr")).open("xb")
        os.chmod(stdout.name, 0o600)
        os.chmod(stderr.name, 0o600)
        environment = os.environ.copy()
        for key in ["NOTIFY_SOCKET", "WATCHDOG_USEC"]:
            environment.pop(key, None)
        child = subprocess.Popen([str(binary), *arguments], stdout=stdout, stderr=stderr, env=environment)
        self.children.append(child)
        self.logs.extend([stdout, stderr])
        return child

    def start_daemon(self, label):
        daemon = self.start(self.bridge, ["serve", "--config", str(self.output / "bridge.toml"), "--no-ui"], label)
        def ready():
            require(daemon.poll() is None, "actual daemon exited before readiness")
            try:
                with socket.create_connection(("127.0.0.1", self.management), timeout=0.2):
                    return True
            except OSError:
                return False
        wait_for(ready, "actual daemon readiness deadline")
        return daemon

    def http(self, path, grant="privileged", body=None):
        data = None if body is None else json.dumps(body).encode()
        request = urllib.request.Request(f"http://127.0.0.1:{self.management}" + path, data=data,
                                         headers={"Authorization": "Bearer " + self.grants[grant], "Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(request, timeout=3) as response:
                return response.status, json.load(response)
        except urllib.error.HTTPError as error:
            return error.code, json.loads(error.read())

    def await_station(self, sim, prior_boots):
        def registered():
            require(sim.poll() is None, "independent simulator exited before admission")
            status, station = self.http("/api/v1/stations/alpha", grant="read")
            require(status not in [401, 403], "station read principal was not authorized")
            accepted = any(point.get("point_id") == "ocpp201/registration/status"
                           and point.get("value") == {"type": "text", "value": "Accepted"}
                           for point in station.get("current_values", []))
            return status == 200 and station.get("connectivity", {}).get("status") == "connected" and accepted and len(self.relay.accepted_boots()) > prior_boots
        wait_for(registered, "real current-socket Accepted Boot deadline")

    def command(self, name, action, update=None, grant="privileged", uncertain=False):
        payload = {}
        schema = "urn:OCPP:Cp:2:2020:3:" + action + "Request"
        if update is not None:
            schema = "urn:uob:ocpp201:SendLocalListReference:1"
            native = next(entry["request"] for entry in self.updates if entry["update_reference"] == self.references[update])
            payload = {"versionNumber": native["versionNumber"], "updateType": native["updateType"], "updateReference": self.references[update]}
        body = {"request_id": name, "resource": {"bridge_id": "joint-112", "station_id": "alpha"},
                "operation": {"kind": "ocpp", "parameters": {"protocol": "ocpp201", "action": action, "payload_schema": schema, "payload": payload}},
                "expires_at": future_expiry()}
        status, result = self.http("/api/v1/commands", grant, body)
        if grant != "privileged":
            require(status in [401, 403], "nonprivileged command was not denied")
            self.evidence["checks"].append("nonprivileged management denial")
            return None
        require(status == 202, "privileged command admission failed")
        def settled():
            nonlocal result
            status, result = self.http("/api/v1/commands/" + name, grant="read")
            require(status == 200, "command status read principal unavailable")
            return result.get("lifecycle", {}).get("stage") == "transmission_uncertain" if uncertain else result.get("local_authorization_201") is not None
        wait_for(settled, "real native command did not settle", seconds=40 if uncertain else 10)
        if not uncertain:
            require(result["schema_version"]["revision"] == 10, "native result revision mismatch")
        require(not any(marker in json.dumps(result).encode().lower() for marker in self.markers), "private command result marker")
        self.evidence["checks"].append(name)
        return result if uncertain else result["local_authorization_201"]

    def snapshots(self):
        database = self.output / "service-state/charging.sqlite3"
        if not database.exists():
            return []
        with sqlite3.connect(f"file:{database}?mode=ro", uri=True) as connection:
            return [json.loads(row[0]) for row in connection.execute("SELECT payload FROM station_snapshots")]

    def original(self):
        return [tx for snapshot in self.snapshots() if snapshot["station"]["station_id"] == "alpha"
                for tx in snapshot.get("transactions", []) if tx.get("protocol_state", {}).get("native_transaction_id") == "native-offline-112"]

    def run(self):
        daemon = self.start_daemon("daemon")
        prior_boots = len(self.relay.accepted_boots())
        sim = self.start(self.simulator, ["run", "--config", str(self.output / "sim.toml"), "--scenario", str(self.output / "scenario.toml"), "--format", "jsonl"], "simulator")
        self.await_station(sim, prior_boots)
        self.command("denied-read", "GetLocalListVersion", grant="read")
        require(self.command("query-fresh-zero", "GetLocalListVersion")["version_number"] == 0, "fresh native list did not query zero")
        require(self.command("install-full", "SendLocalList", "full")["status"] == "Accepted", "Full was not accepted")
        def originals_delivered():
            require(sim.poll() is None, "independent simulator exited before original delivery")
            state = json.loads((self.output / "sim-state.json").read_text())
            transactions = self.original()
            return state["version"] == 7 and not state["offline"] and not state["active"] and any(
                tx["state"] == "ended" and tx["protocol_state"]["last_event"] == "Ended"
                and tx["protocol_state"]["last_sequence_number"] == 1 for tx in transactions)
        wait_for(originals_delivered, "original native offline facts did not deliver")
        self.evidence["checks"].append("independent list-based offline type/EVSE denials and original delivery after Accepted Boot")
        require(self.command("query-installed", "GetLocalListVersion")["version_number"] == 7, "installed version mismatch")
        daemon = run_strengthened(self, daemon, sim, require, wait_for, private)
        status, history = self.http("/api/v1/commands?station_id=alpha", grant="read")
        require(status == 200 and not any(marker in json.dumps(history).encode().lower() for marker in self.markers), "private command history marker")
        private(self.output / "command-history.json", json.dumps(history, indent=2))
        self.evidence["checks"].append("authenticated command history excludes native metadata")
        self.stop()
        original = self.original()
        require(len(original) == 1, "original native transaction was not durably committed")
        native = original[0]["protocol_state"]
        require(native["last_sequence_number"] == 1 and native["last_event"] == "Ended", "original sequence/end fact changed")
        require(native["native_resource"]["evse_id"] == 1 and native["native_resource"]["connector_id"] == 1, "original EVSE changed")
        require(original[0]["started_at"] == self.started_at and original[0]["ended_at"] == self.ended_at, "original timestamps changed")
        self.evidence["checks"].append("daemon SQLite commits original transaction ID/EVSE/sequence/timestamps")
        for path in self.output.rglob("*"):
            if path.is_file() and (path.suffix in [".stdout", ".stderr"] or self.output / "service-state" in path.parents):
                require(not any(marker in path.read_bytes().lower() for marker in self.markers), "private marker in service DB/WAL or process outputs")
        self.evidence["checks"].append("service SQLite/WAL and all process outputs exclude secret markers")
        private(self.output / "smoke.json", json.dumps(self.evidence, indent=2) + "\n")

    def stop(self):
        for child in self.children:
            if child.poll() is None:
                child.terminate()
                try:
                    child.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait(timeout=10)
        self.relay.close()
        for log in self.logs:
            if not log.closed:
                log.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bridge", type=Path, required=True)
    parser.add_argument("--simulator", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    bridge, simulator, output = args.bridge.resolve(), args.simulator.resolve(), args.output.resolve()
    require(bridge.is_file() and simulator.is_file(), "built actual executables required")
    output.mkdir(mode=0o700)
    smoke = Smoke(bridge, simulator, output)
    try:
        smoke.run()
    finally:
        smoke.stop()
    print("native201 actual joint smoke passed; private evidence written")


if __name__ == "__main__":
    main()
