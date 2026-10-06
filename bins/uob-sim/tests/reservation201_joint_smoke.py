#!/usr/bin/env python3
"""Opt-in independent actual uob/uob-sim OCPP 2.0.1 reservation proof, synthetic loopback only."""
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
import urllib.error
import urllib.request
from reservation16_joint_flow import scenario_document
from reservation201_joint_flow import run_flow, step, boot, parked
from reservation201_joint_transport import NativeWireRelay201


def require(condition, label):
    if not condition:
        raise RuntimeError(label)


def private(path, text):
    with path.open("x", encoding="utf-8") as handle:
        os.chmod(path, 0o600)
        handle.write(text)
    return path


def port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def date(seconds=120):
    return (datetime.now(timezone.utc) + timedelta(seconds=seconds)).isoformat(timespec="milliseconds").replace("+00:00", "Z")


def wait_for(predicate, label, seconds=10):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.025)
    raise RuntimeError(label)


RESERVATIONS = [
    # name, evseId (None = unspecified), reservation id, expiry seconds
    ("exact", 1, -114, 300), ("occupied", 1, 0, 300), ("replacement", 2, -114, 300),
    ("unspecified", None, 7, 300), ("use", 1, 114, 300), ("expiry", 1, 115, 8),
    ("drop", 1, 117, 300), ("min", 2, -2147483648, 300), ("max", 1, 2147483647, 300),
]


class Smoke:
    def __init__(self, bridge, simulator, output):
        self.bridge, self.simulator, self.output = bridge, simulator, output
        self.children, self.logs = [], []
        self.management, self.charging = port(), port()
        while self.management == self.charging:
            self.charging = port()
        self.relay = NativeWireRelay201(self.charging)
        self.grants = {name: "uob1.demo." + secrets.token_hex(16) for name in ["read", "control", "privileged"]}
        for name, grant in self.grants.items():
            private(output / name, grant)
        self.owner, self.member, self.group = "token-marker-114", "member-marker-114", "group-marker-114"
        self.secret = "station-marker-114"
        self.markers = [value.encode() for value in [self.owner, self.member, self.group, self.secret, *self.grants.values()]]
        private(output / "station-auth", self.secret)
        self.requests, self.references = {}, {}
        for name, evse, identifier, seconds in RESERVATIONS:
            request = {"id": identifier, "expiryDateTime": date(seconds),
                       "idToken": {"idToken": self.owner, "type": "ISO14443"},
                       "groupIdToken": {"idToken": self.group, "type": "Central"}}
            if evse is not None:
                request["evseId"] = evse
            self.requests[name] = request
            self.references[name] = "reserve201:" + secrets.token_hex(32)
        provisioning = {"reservations": [{"reference": self.references[name], "request": request,
                                          "expires_at": "2099-01-01T00:00:00Z", "revoked": False}
                                         for name, request in self.requests.items()],
                        "identities": [
                            {"idToken": {"idToken": self.owner, "type": "ISO14443"},
                             "groupIdToken": {"idToken": self.group, "type": "Central"}, "authorize": True},
                            {"idToken": {"idToken": self.member, "type": "ISO14443"},
                             "groupIdToken": {"idToken": self.group, "type": "Central"}, "authorize": True}]}
        private(output / "reservations.json", json.dumps(provisioning))
        state = output / "service-state"
        state.mkdir(mode=0o700)
        private(output / "bridge.toml", f"""[bridge]
id='joint-114'
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
[[charging.stations]]
id='alpha'
protocol='ocpp201'
credential_file='{output / 'station-auth'}'
reservation201_file='{output / 'reservations.json'}'
reserve_now=true
cancel_reservation=true
reserve_non_evse_specific_supported=true
[[charging.stations.resources]]
evse_id='one'
native_evse_id=1
[[charging.stations.resources]]
evse_id='one'
native_evse_id=1
connector_id='one'
native_connector_id=1
[[charging.stations.resources]]
evse_id='two'
native_evse_id=2
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
request_timeout_ms=45000
reconnect=false
[[stations.evses]]
id=1
connectors=[1]
[[stations.evses]]
id=2
connectors=[1]
[stations.local_authorization]
private_state_file='{output / 'sim-identity.json'}'
list_supported=true
cache_supported=true
[stations.reservation201]
private_state_file='{output / 'sim-reservations.json'}'
enabled=true
non_evse_specific=true
""")
        self.evidence = {"checks": [], "executables": {
            "bridge_sha256": hashlib.sha256(bridge.read_bytes()).hexdigest(),
            "simulator_sha256": hashlib.sha256(simulator.read_bytes()).hexdigest()}}
        self.counter = 0
        self.sim = None

    def check(self, label):
        self.evidence["checks"].append(label)

    def start(self, binary, args, label):
        stdout = (self.output / (label + ".stdout")).open("xb")
        stderr = (self.output / (label + ".stderr")).open("xb")
        os.chmod(stdout.name, 0o600)
        os.chmod(stderr.name, 0o600)
        environment = os.environ.copy()
        for key in ["NOTIFY_SOCKET", "WATCHDOG_USEC"]:
            environment.pop(key, None)
        child = subprocess.Popen([str(binary), *args], stdout=stdout, stderr=stderr, env=environment)
        self.children.append(child)
        self.logs.extend([stdout, stderr])
        return child

    def daemon(self):
        self.counter += 1
        child = self.start(self.bridge, ["serve", "--config", str(self.output / "bridge.toml"), "--no-ui"],
                           "daemon-" + str(self.counter))
        def ready():
            require(child.poll() is None, "actual bridge exited before readiness")
            try:
                with socket.create_connection(("127.0.0.1", self.management), timeout=0.1):
                    return True
            except OSError:
                return False
        wait_for(ready, "actual daemon readiness")
        return child

    def http(self, path, grant="privileged", body=None, timeout=3):
        data = None if body is None else json.dumps(body).encode()
        request = urllib.request.Request(f"http://127.0.0.1:{self.management}" + path, data=data,
            headers={"Authorization": "Bearer " + self.grants[grant], "Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(request, timeout=timeout) as response:
                return response.status, json.load(response)
        except urllib.error.HTTPError as error:
            return error.code, json.loads(error.read())

    def station(self, steps=None):
        if self.sim and self.sim.poll() is None:
            self.sim.kill()
            self.sim.wait(timeout=10)
        self.counter += 1
        steps = [step("connect", "connect"), boot(), *(steps or []), parked()]
        setup = 0
        for entry in steps:
            if entry["action"] in ["await_reservation", "start_transaction"]:
                break
            if entry["action"] in ["status", "authorize"]:
                setup += 1
        path = private(self.output / f"scenario-{self.counter}.toml", scenario_document(steps))
        boots = len(self.relay.accepted_boots())
        self.sim = self.start(self.simulator, ["run", "--config", str(self.output / "sim.toml"), "--scenario",
                                               str(path), "--format", "jsonl"], f"simulator-{self.counter}")
        wait_for(lambda: self.sim.poll() is None and len(self.relay.accepted_boots()) > boots,
                 "real fresh Accepted Boot before management mutation")
        generation = self.relay.accepted_boots()[-1]
        wait_for(lambda: len([call for call in self.relay.observed("station") if call["connection"] == generation
                              and call["action"] in ["StatusNotification", "Authorize"]
                              and "reply_elapsed_ms" in call]) >= setup,
                 "actual native setup exchanges did not finish")
        return generation

    def command(self, name, request=None, action="ReserveNow", identifier=None, grant="privileged",
                uncertain=False, override=None, deny=False, scope=None, schema=None, status=None):
        resource = {"bridge_id": "joint-114", "station_id": "alpha"}
        if action == "ReserveNow":
            native = self.requests[request or name]
            payload = {"id": native["id"], "expiryDateTime": native["expiryDateTime"],
                       "reservationReference": self.references[request or name]}
            if "evseId" in native:
                payload["evseId"] = native["evseId"]
                resource["resource"] = {"kind": "evse", "evse_id": "one" if native["evseId"] == 1 else "two"}
                resource["native_protocol_reference"] = {"protocol": "ocpp201", "evse_id": native["evseId"]}
            schema = schema or "urn:uob:ocpp201:ReserveNowReference:1"
        else:
            payload = {"reservationId": identifier}
            schema = schema or "urn:OCPP:Cp:2:2020:3:CancelReservationRequest"
        payload.update(override or {})
        resource.update(scope or {})
        body = {"request_id": name, "resource": resource, "operation": {"kind": "ocpp", "parameters": {
            "protocol": "ocpp201", "action": action, "payload_schema": schema, "payload": payload}},
            "expires_at": date(90)}
        before = len(self.relay.mutations())
        code, result = self.http("/api/v1/commands", grant, body, timeout=45 if uncertain else 5)
        if grant != "privileged" or deny:
            require(code in [400, 401, 403, 404, 409, 410, 422], f"invalid command admitted: {name} ({code})")
            if grant != "privileged":
                require(code in [401, 403], "permission denial did not remain a permission error")
            self.private_text(json.dumps(result), "denied management result")
            require(len(self.relay.mutations()) == before, "denied command reached native wire")
            self.check(name)
            return None
        require(code == 202, f"privileged reservation admission failed: {name} ({code} {json.dumps(result)})")
        def settled():
            nonlocal result
            code, result = self.http("/api/v1/commands/" + name, "read")
            require(code == 200, "real command result unavailable")
            if uncertain:
                return result.get("lifecycle", {}).get("stage") == "transmission_uncertain"
            return result.get("reservation_201", {}).get("status") is not None
        wait_for(settled, "actual command did not settle: " + name, 40 if uncertain else 10)
        self.private_text(json.dumps(result), "public command result")
        evidence = result["reservation_201"]
        if uncertain:
            require(not evidence.get("status"), "lost ACK invented native status")
        else:
            require(result["schema_version"]["revision"] == 12, "reservation result revision")
            require(evidence["action"] == action, "action-tagged native result")
            expected = self.requests[request or name]["id"] if action == "ReserveNow" else identifier
            require(evidence["reservation_id"] == expected, "signed reservation identity changed")
            require(status is None or evidence["status"] == status, f"{name} native status {evidence['status']}")
        self.check(name)
        return result

    def private_text(self, text, label):
        require(not any(marker in text.encode().lower() for marker in self.markers), "raw marker in " + label)

    def records(self):
        path = self.output / "service-state/charging.sqlite3"
        with sqlite3.connect(f"file:{path}?mode=ro", uri=True) as connection:
            return [json.loads(row[0]) for row in connection.execute("SELECT payload FROM reservations201 ORDER BY revision")]

    def workflow(self, identifier, state, seconds=10):
        def observed():
            rows = [r for r in self.records() if r["reservation_id"] == identifier]
            return bool(rows) and rows[-1]["state"] == state
        wait_for(observed, f"durable reservation {identifier} did not become {state}", seconds)
        self.check(f"durable {identifier} {state}")
        return [r for r in self.records() if r["reservation_id"] == identifier]

    def close(self):
        self.relay.close()
        for child in self.children:
            if child.poll() is None:
                child.kill()
            child.wait(timeout=10)
        for handle in self.logs:
            handle.close()

    def run(self):
        daemon = self.daemon()
        run_flow(self, daemon, require, wait_for)
        code, history = self.http("/api/v1/commands?station_id=alpha", "read")
        require(code == 200, "authenticated history unavailable")
        self.private_text(json.dumps(history), "history")
        for path in [*self.output.glob("*.stdout"), *self.output.glob("*.stderr"),
                     *self.output.glob("service-state/charging.sqlite3*")]:
            require(not any(marker in path.read_bytes().lower() for marker in self.markers),
                    "raw marker leaked to public logs/SQLite/WAL: " + path.name)
        self.check("public results/history/logs/SQLite/WAL raw-marker privacy")
        self.evidence["native_wire"] = self.relay.mutations()
        private(self.output / "smoke.json", json.dumps(self.evidence, indent=2) + "\n")
        print(f"reservation201 joint smoke passed {len(self.evidence['checks'])} checks")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bridge", type=Path, required=True)
    parser.add_argument("--simulator", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    require(args.bridge.is_file() and args.simulator.is_file(), "fresh binaries are required")
    require(not args.output.exists(), "output must be a fresh private directory")
    args.output.mkdir(mode=0o700)
    smoke = Smoke(args.bridge.resolve(), args.simulator.resolve(), args.output.resolve())
    try:
        smoke.run()
    finally:
        smoke.close()


if __name__ == "__main__":
    main()
