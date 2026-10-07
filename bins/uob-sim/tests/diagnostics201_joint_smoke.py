#!/usr/bin/env python3
"""Opt-in actual uob/uob-sim OCPP 2.0.1 GetLog upload proof, synthetic loopback only."""
import argparse
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


def require(condition, label):
    if not condition:
        raise RuntimeError(label)


def private(path, data):
    with path.open("xb") as handle:
        os.chmod(path, 0o600)
        handle.write(data if isinstance(data, bytes) else data.encode())
    return path


def port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def wait_for(predicate, label, seconds=20):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.05)
    raise RuntimeError(label)


def listening(number):
    try:
        with socket.create_connection(("127.0.0.1", number), timeout=0.1):
            return True
    except OSError:
        return False


# alpha uploads both log types; bravo is interrupted by a daemon restart before its upload;
# charlie's stalled upload is cancelled by a newer GetLog (N01.FR.12 and N01.FR.20).
STATIONS = {
    "alpha": {"sim": "status_delay_ms=200",
              "steps": [("diagnostics-log", "DiagnosticsLog", "Uploaded", 1), ("security-log", "SecurityLog", "Uploaded", 2)]},
    "bravo": {"sim": "status_delay_ms=3000", "steps": [("failed", "DiagnosticsLog", "UploadFailure", 0)]},
    "charlie": {"sim": "upload_failures=1", "steps": [("replacement", "SecurityLog", "Uploaded", 1)]},
}
SCHEMA = "urn:uob:ocpp201:GetLogReference:1"
DIAGNOSTICS_BYTES, SECURITY_LOG_BYTES = 6144, 3072


class Smoke:
    def __init__(self, bridge, simulator, output):
        self.bridge, self.simulator, self.output = bridge, simulator, output
        self.children, self.logs, self.checks = [], [], []
        self.management, self.charging, self.artifacts = port(), port(), port()
        require(len({self.management, self.charging, self.artifacts}) == 3, "distinct ports")
        self.grants = {name: "uob1.demo." + secrets.token_hex(16) for name in ["read", "control", "privileged"]}
        for name, grant in self.grants.items():
            private(output / name, grant)
        self.secret = "station-marker-" + secrets.token_hex(8)
        for name in STATIONS:
            private(output / f"station-{name}-auth", self.secret + "-" + name)
        (output / "spool").mkdir(mode=0o700)
        (output / "service-state").mkdir(mode=0o700)
        stations = "".join(f"""[[charging.stations]]
id='{name}'
protocol='ocpp201'
credential_file='{output / f'station-{name}-auth'}'
get_log=true
diagnostics_job_timeout_seconds=600
diagnostics_upload_max_bytes=65536
[[charging.stations.resources]]
evse_id='one'
native_evse_id=1
[[charging.stations.resources]]
evse_id='one'
native_evse_id=1
connector_id='one'
native_connector_id=1
""" for name in STATIONS)
        private(output / "bridge.toml", f"""[bridge]
id='joint-125'
environment='demo'
[management]
listen_addr='127.0.0.1:{self.management}'
[charging]
enabled=true
listen_addr='127.0.0.1:{self.charging}'
state_directory='{output / 'service-state'}'
read_grant_file='{output / 'read'}'
control_grant_file='{output / 'control'}'
privileged_grant_file='{output / 'privileged'}'
[charging.firmware]
listen_addr='127.0.0.1:{self.artifacts}'
spool_directory='{output / 'spool'}'
{stations}""")

    def start(self, binary, args, label):
        stdout = (self.output / (label + ".stdout")).open("xb")
        stderr = (self.output / (label + ".stderr")).open("xb")
        os.chmod(stdout.name, 0o600)
        os.chmod(stderr.name, 0o600)
        environment = {key: value for key, value in os.environ.items()
                       if key not in ["NOTIFY_SOCKET", "WATCHDOG_USEC"]}
        child = subprocess.Popen([str(binary), *args], stdout=stdout, stderr=stderr, env=environment)
        self.children.append(child)
        self.logs.extend([stdout, stderr])
        return child

    def daemon(self, label):
        child = self.start(self.bridge, ["serve", "--config", str(self.output / "bridge.toml"), "--no-ui"], label)
        wait_for(lambda: child.poll() is None and listening(self.management) and listening(self.artifacts),
                 "actual daemon readiness")
        return child

    def station(self, name):
        station = STATIONS[name]
        config = private(self.output / f"sim-{name}.toml", f"""schema_version=1
[[stations]]
id='{name}'
endpoint='ws://127.0.0.1:{self.charging}/ocpp/{name}'
ocpp_version='2.0.1'
credentials_file='{self.output / f'station-{name}-auth'}'
request_timeout_ms=10000
reconnect=true
[[stations.evses]]
id=1
connectors=[1]
[stations.diagnostics201]
private_state_file='{self.output / f'sim-{name}-diagnostics.json'}'
{station['sim']}
upload_timeout_ms=5000
diagnostics_bytes={DIAGNOSTICS_BYTES}
security_log_bytes={SECURITY_LOG_BYTES}
""")
        steps = "".join(f"""[[steps]]
id='{step}'
station='{name}'
action='await_diagnostics'
timeout_ms=90000
expect_response={{active=false,kind='{kind}',lastStatus='{last}',uploads={uploads},pendingStatuses=0}}
""" for step, kind, last, uploads in station["steps"])
        scenario = private(self.output / f"scenario-{name}.toml", f"""schema_version=1
seed=125
[[steps]]
id='connect'
station='{name}'
action='connect'
timeout_ms=5000
[[steps]]
id='boot'
station='{name}'
action='boot'
timeout_ms=5000
fixture_id='sim.ocpp201.boot.accepted'
payload={{reason='PowerUp',chargingStation={{vendorName='UOB',model='DiagnosticsSt201',firmwareVersion='sim-1.0.0'}}}}
{steps}[[steps]]
id='disconnect'
station='{name}'
action='disconnect'
timeout_ms=5000
""")
        child = self.start(self.simulator, ["run", "--config", str(config), "--scenario", str(scenario),
                                            "--format", "jsonl"], "simulator-" + name)
        wait_for(lambda: self.registered(name), f"{name} accepted BootNotification")
        return child

    def http(self, path, grant="privileged", body=None):
        data = None if body is None else json.dumps(body).encode()
        request = urllib.request.Request(f"http://127.0.0.1:{self.management}" + path, data=data,
            headers={"Authorization": "Bearer " + self.grants[grant], "Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(request, timeout=40) as response:
                return response.status, json.load(response)
        except urllib.error.HTTPError as error:
            return error.code, json.loads(error.read() or b"null")
        except (urllib.error.URLError, ConnectionError):
            return 0, None

    def registered(self, name):
        status, snapshot = self.http("/api/v1/stations/" + name, "read")
        return status == 200 and any(value.get("point_id") == "ocpp201/registration/status"
                                     and value.get("value", {}).get("value") == "Accepted"
                                     for value in snapshot.get("current_values", []))

    def submit(self, request_id, name, log_type, request_number, retry_interval=1, expect="Accepted"):
        payload = {"logType": log_type, "requestId": request_number, "retries": 1, "retryInterval": retry_interval}
        body = {"request_id": request_id, "resource": {"bridge_id": "joint-125", "station_id": name},
                "operation": {"kind": "ocpp", "parameters": {"protocol": "ocpp201", "action": "GetLog",
                              "payload_schema": SCHEMA, "payload": payload}},
                "expires_at": "2099-01-01T00:00:00Z"}
        status, response = self.http("/api/v1/commands", body=body)
        require(status == 202, f"{request_id} admission: {status} {json.dumps(response)}")
        result = response["result"]
        require(result["lifecycle"]["accepted"] is True, f"{request_id} native acceptance")
        require(result["schema_version"]["revision"] == 17, "command-result revision 17")
        evidence = result["diagnostics_201"]
        require(evidence["request_id"] == request_number and evidence["log_type"] == log_type, "native identity")
        require(evidence["reply"]["kind"] == "status" and evidence["reply"]["status"] == expect, "exact native reply")
        require(evidence["reply"].get("file_name"), f"{request_id} named its file")
        require(evidence["destination"]["test_only"] is True, "test destination marking")
        self.private_result(result)
        self.checks.append(f"{request_id} {expect} with a test-only destination")
        return result

    def private_result(self, value):
        text = json.dumps(value)
        for marker in ["/uploads/", "http://", self.secret, *self.grants.values()]:
            require(marker not in text, "private material in public result")

    def job(self, request_id):
        status, result = self.http("/api/v1/commands/" + request_id, "read")
        return result["diagnostics_201"]["job"] if status == 200 else None

    def reach(self, request_id, state, seconds=60):
        wait_for(lambda: (self.job(request_id) or {}).get("state") == state,
                 f"{request_id} did not reach {state}: {self.job(request_id)}", seconds)
        self.checks.append(f"{request_id} reached {state}")
        return self.job(request_id)

    def release_jobs(self):
        path = self.output / "service-state/charging.sqlite3"
        with sqlite3.connect(f"file:{path}?mode=ro", uri=True) as connection:
            return connection.execute("SELECT COUNT(*) FROM release_jobs WHERE kind='diagnostics'").fetchone()[0]

    def run(self):
        daemon = self.daemon("daemon-1")
        alpha, bravo, charlie = self.station("alpha"), self.station("bravo"), self.station("charlie")
        self.submit("diagnostics-log-1", "alpha", "DiagnosticsLog", 125)
        job = self.reach("diagnostics-log-1", "uploaded")
        require(job["upload"]["size_bytes"] == DIAGNOSTICS_BYTES, "provider stored the whole diagnostics log")
        require(job["last_status"] == "Uploaded" and job["notifications"] == 2, "clean native sequence")
        self.submit("security-log-1", "alpha", "SecurityLog", 126)
        job = self.reach("security-log-1", "uploaded")
        require(job["upload"]["size_bytes"] == SECURITY_LOG_BYTES, "provider stored the whole security log")
        self.checks.append("bridge-observed upload sizes match the independent station's generated files")
        # charlie's first attempt fails and waits 30 s to retry, so its upload is still ongoing.
        self.submit("cancelled-1", "charlie", "SecurityLog", 127, retry_interval=30)
        wait_for(lambda: (self.job("cancelled-1") or {}).get("state") == "uploading", "charlie upload started", 30)
        self.submit("replacement-1", "charlie", "SecurityLog", 128, expect="AcceptedCanceled")
        self.reach("cancelled-1", "cancelled")
        job = self.reach("replacement-1", "uploaded")
        require(job["upload"]["size_bytes"] == SECURITY_LOG_BYTES, "replacement upload stored")
        self.checks.append("a newer GetLog cancelled the ongoing upload and its replacement uploaded")
        self.submit("interrupted-1", "bravo", "DiagnosticsLog", 129)
        require(self.release_jobs() == 1, "unresolved upload job holds the release drain")
        daemon.kill()
        daemon.wait(timeout=10)
        self.checks.append("daemon killed before the bravo upload")
        daemon = self.daemon("daemon-2")
        job = self.reach("interrupted-1", "upload_failed", 90)
        require(job.get("upload") is None, "no provider facts for a failed upload")
        require(job["last_status"] == "UploadFailure", "native failure status preserved")
        require(self.release_jobs() == 0, "resolved jobs release the drain")
        for station in ["alpha", "bravo", "charlie"]:
            status, history = self.http("/api/v1/commands?station_id=" + station, "read")
            require(status == 200, "history")
            self.private_result(history)
        for child, name in [(alpha, "alpha"), (bravo, "bravo"), (charlie, "charlie")]:
            require(child.wait(timeout=90) == 0, f"simulator {name} scenario completed")
        # Statuses of each station's latest upload, as its independent model sent them.
        expected = {"alpha": ("security-log", ["Uploading", "Uploaded"]),
                    "charlie": ("replacement", ["Uploading", "Uploading", "Uploaded"])}
        for name, (step_id, statuses) in expected.items():
            events = [json.loads(line) for line in (self.output / f"simulator-{name}.stdout").read_text().splitlines()]
            snapshot = json.loads(next(event["detail"] for event in events
                                       if event.get("step_id") == step_id and event["event"] == "step_passed"))
            require(snapshot["statuses"] == statuses, f"{name} native sequence {snapshot['statuses']}")
        self.checks.append("independent simulators uploaded with exact native status sequences")
        for path in self.output.glob("*.std*"):
            require(self.secret.encode() not in path.read_bytes(), "station secret in logs")
        private(self.output / "smoke.json", json.dumps({"checks": self.checks}, indent=2) + "\n")
        print(f"2.0.1 diagnostics joint smoke passed {len(self.checks)} checks")

    def close(self):
        for child in self.children:
            if child.poll() is None:
                child.kill()
            child.wait(timeout=10)
        for handle in self.logs:
            handle.close()


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
