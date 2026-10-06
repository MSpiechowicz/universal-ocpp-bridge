#!/usr/bin/env python3
"""Opt-in independent actual uob/uob-sim reservation proof, synthetic loopback only."""
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
from reservation16_joint_flow import run_flow, scenario_document, step, boot, parked, same_instant
from reservation16_joint_transport import NativeWireRelay


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


class Smoke:
    def __init__(self, bridge, simulator, output):
        self.bridge, self.simulator, self.output = bridge, simulator, output
        self.children, self.logs = [], []
        self.management, self.charging = port(), port()
        while self.management == self.charging:
            self.charging = port()
        self.relay = NativeWireRelay(self.charging)
        self.grants = {name: "uob1.demo." + secrets.token_hex(16) for name in ["read", "control", "privileged"]}
        for name, grant in self.grants.items():
            private(output / name, grant)
        self.owner, self.member, self.group = "token-marker-113", "member-marker-113", "group-marker-113"
        self.denied = "denied-marker-113"
        self.secret = "station-marker-113"
        self.markers = [value.encode() for value in [self.owner, self.member, self.group, self.denied, self.secret, *self.grants.values()]]
        private(output / "station-auth", self.secret)
        self.requests, self.references = {}, {}
        self.identities = [{"idTag": token, "parentIdTag": self.group, "authorize": True} for token in [self.owner, self.member, self.denied]]
        self.other = "other-marker-113"
        self.markers.append(self.other.encode())
        self.markers.append(b"other-parent-113")
        self.identities.extend([{"idTag": self.other, "parentIdTag": "other-parent-113", "authorize": True},
                                {"idTag": self.group, "authorize": False}])
        for name, connector, identifier in [
                ("exact", 1, -113), ("replacement", 2, -113), ("occupied", 1, 0),
                ("faulted", 1, -2147483648), ("unavailable", 1, 2147483647),
                ("any", 0, 0), ("group", 1, 113), ("denied", 2, 114),
                ("expiry", 1, 115), ("delayed", 1, 116), ("drop", 0, 117),
                ("late-expiry", 1, 118), ("renewal", 2, 115)]:
            token = self.denied if name == "denied" else self.owner
            expiry = date(8 if name in ["expiry", "late-expiry"] else 300)
            self.requests[name] = {"connectorId": connector, "expiryDate": expiry, "idTag": token,
                                   "parentIdTag": self.group, "reservationId": identifier}
            self.references[name] = "reserve16:" + secrets.token_hex(32)
        self.write_provisioning()
        state = output / "service-state"
        state.mkdir(mode=0o700)
        private(output / "bridge.toml", f"""[bridge]
id='joint-113'
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
protocol='ocpp16j'
credential_file='{output / 'station-auth'}'
reservation16_file='{output / 'reservations.json'}'
reserve_now=true
cancel_reservation=true
reserve_connector_zero_supported=true
[[charging.stations.resources]]
connector_id='one'
native_connector_id=1
[[charging.stations.resources]]
connector_id='two'
native_connector_id=2
""")
        private(output / "sim.toml", f"""schema_version=1
[[stations]]
id='alpha'
endpoint='ws://127.0.0.1:{self.relay.port}/ocpp/alpha'
ocpp_version='1.6'
credentials_file='{output / 'station-auth'}'
connectors=[1,2]
request_timeout_ms=45000
reconnect=false
[stations.local_authorization]
private_state_file='{output / 'sim-identity.json'}'
[stations.reservation16]
private_state_file='{output / 'sim-reservations.json'}'
enabled=true
reserve_connector_zero_supported=true
""")
        self.evidence = {"checks": [], "executables": {
            "bridge_sha256": hashlib.sha256(bridge.read_bytes()).hexdigest(),
            "simulator_sha256": hashlib.sha256(simulator.read_bytes()).hexdigest()}}
        self.counter = 0
        self.sim = None
        self.stops = 0

    def write_provisioning(self):
        value = {"reservations": [{"reference": self.references[name], "request": request,
                                  "expires_at": "2099-01-01T00:00:00Z", "revoked": False}
                                 for name, request in self.requests.items()], "identities": self.identities}
        path = self.output / "reservations.json"
        if path.exists():
            # This is an explicit owner provisioning change, not a test-only policy bypass.
            with path.open("w", encoding="utf-8") as handle:
                json.dump(value, handle)
                handle.flush()
                os.fsync(handle.fileno())
        else:
            private(path, json.dumps(value))

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

    def daemon(self, config="bridge.toml"):
        self.counter += 1
        child = self.start(self.bridge, ["serve", "--config", str(self.output / config), "--no-ui"], "daemon-" + str(self.counter))
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

    def station(self, steps=None, pre=None, config="sim.toml"):
        if self.sim and self.sim.poll() is None and self.stops:
            # A stop frees its connector only once the service has committed and replied.
            wait_for(lambda: len([call for call in self.relay.calls if call["action"] == "StopTransaction"
                                  and "reply_elapsed_ms" in call]) >= self.stops,
                     "actual StopTransaction was not acknowledged")
        if self.sim and self.sim.poll() is None:
            self.sim.kill()
            self.sim.wait(timeout=10)
        self.counter += 1
        steps = [step("connect", "connect"), *(pre or []), boot(), *(steps or []), parked()]
        self.stops += sum(entry["action"] == "stop_transaction" for entry in steps)
        setup_count = 0
        for entry in steps:
            if entry["action"] == "await_reservation":
                break
            if entry["action"] in ["status", "authorize"]:
                setup_count += 1
        path = private(self.output / f"scenario-{self.counter}.toml", scenario_document(steps))
        boots = len(self.relay.accepted_boots())
        self.sim = self.start(self.simulator, ["run", "--config", str(self.output / config), "--scenario", str(path), "--format", "jsonl"], f"simulator-{self.counter}")
        wait_for(lambda: self.sim.poll() is None and len(self.relay.accepted_boots()) > boots,
                 "real fresh Accepted Boot before management mutation")
        generation = self.relay.accepted_boots()[-1]
        wait_for(lambda: len([call for call in self.relay.calls if call["connection"] == generation
                              and call["direction"] == "station" and call["action"] in ["StatusNotification", "Authorize"]
                              and "reply_elapsed_ms" in call]) >= setup_count,
                 "actual native setup exchanges did not finish")
        return self.sim

    def command(self, name, action="ReserveNow", request="exact", identifier=None, grant="privileged", uncertain=False, override=None, deny=False, deadline=None, scope=None):
        resource = {"bridge_id": "joint-113", "station_id": "alpha"}
        if action == "ReserveNow":
            native = self.requests[request]
            payload = {key: native[key] for key in ["connectorId", "expiryDate", "reservationId"]}
            payload["reservationReference"] = self.references[request]
            if native["connectorId"]:
                resource["resource"] = {"kind": "connector",
                                        "connector_id": "one" if native["connectorId"] == 1 else "two"}
                resource["native_protocol_reference"] = {"protocol": "ocpp16",
                                                         "connector_id": native["connectorId"]}
            schema = "urn:uob:ocpp16:ReserveNowReference:1"
        else:
            payload = {"reservationId": identifier}
            schema = "urn:OCPP:1.6:2019:12:CancelReservationRequest"
        if override:
            payload.update(override)
        if scope:
            resource.update(scope)
        body = {"request_id": name, "resource": resource, "operation": {"kind": "ocpp", "parameters": {
            "protocol": "ocpp16j", "action": action, "payload_schema": schema, "payload": payload}}, "expires_at": deadline or date(90)}
        before = len(self.relay.mutations())
        # Admission waits for the native reply or the 30s dispatch deadline.
        status, result = self.http("/api/v1/commands", grant, body, timeout=45 if uncertain else 3)
        if grant != "privileged" or deny:
            require(status in [400, 401, 403, 404, 409, 410, 422], "invalid/nonprivileged command unexpectedly admitted")
            if grant != "privileged":
                require(status in [401, 403], "permission denial did not remain a permission error")
            require(not any(marker in json.dumps(result).encode().lower() for marker in self.markers), "raw marker in denied management result")
            require(len(self.relay.mutations()) == before, "denied command reached native wire")
            self.evidence["checks"].append(name)
            return None
        require(status == 202, "privileged reservation command admission failed: %s (%s %s)"
                % (name, status, json.dumps(result)))
        def settled():
            nonlocal result
            status, result = self.http("/api/v1/commands/" + name, "read")
            require(status == 200, "real command result unavailable")
            if uncertain:
                return result.get("lifecycle", {}).get("stage") == "transmission_uncertain"
            return result.get("reservation_16", {}).get("status") is not None
        wait_for(settled, "actual command did not settle: " + name, 40 if uncertain else 10)
        require(not any(marker in json.dumps(result).encode().lower() for marker in self.markers), "raw marker in public command result")
        if uncertain:
            require(not result.get("reservation_16", {}).get("status"), "lost ACK invented native status")
        else:
            require(result["schema_version"]["revision"] == 11, "reservation result revision")
            require(result["reservation_16"]["action"] == action, "action-tagged native result")
            expected_id = native["reservationId"] if action == "ReserveNow" else identifier
            require(result["reservation_16"]["reservation_id"] == expected_id, "signed reservation identity changed")
        self.evidence["checks"].append(name)
        return result

    def records(self):
        path = self.output / "service-state/charging.sqlite3"
        with sqlite3.connect(f"file:{path}?mode=ro", uri=True) as connection:
            return [json.loads(row[0]) for row in connection.execute("SELECT payload FROM reservations16 ORDER BY revision")]

    def workflow(self, identifier, state, source=None):
        def observed():
            rows = [r for r in self.records() if r["station"]["station_id"] == "alpha" and r["reservation_id"] == identifier]
            if not rows or rows[-1]["state"] != state:
                return False
            return source is None or same_instant(rows[-1].get("source_time"), source)
        wait_for(observed, f"durable reservation {identifier} did not become {state}")
        self.evidence["checks"].append(f"durable {identifier} {state}")

    def native(self):
        return json.loads((self.output / "sim-reservations.json").read_text())

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
        run_flow(self, daemon, require, wait_for, private, date)
        status, history = self.http("/api/v1/commands?station_id=alpha", "read")
        require(status == 200, "authenticated history unavailable")
        require(not any(marker in json.dumps(history).encode().lower() for marker in self.markers), "private history marker")
        for path in [*self.output.glob("*.stdout"), *self.output.glob("*.stderr"), *self.output.glob("service-state/charging.sqlite3*")]:
            require(not any(marker in path.read_bytes().lower() for marker in self.markers), "raw marker leaked to public logs/SQLite/WAL")
        self.evidence["checks"].append("public results/history/logs/SQLite/WAL raw-marker privacy")
        self.evidence["native_wire"] = self.relay.mutations()
        private(self.output / "smoke.json", json.dumps(self.evidence, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bridge", type=Path, required=True)
    parser.add_argument("--simulator", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    require(args.bridge.is_file() and args.simulator.is_file(), "fresh binaries are required")
    require(not args.output.exists(), "output must be a fresh private directory")
    args.output.mkdir(mode=0o700)
    output = args.output.resolve()
    smoke = Smoke(args.bridge.resolve(), args.simulator.resolve(), output)
    try:
        smoke.run()
    finally:
        smoke.close()


if __name__ == "__main__":
    main()
