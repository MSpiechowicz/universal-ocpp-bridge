#!/usr/bin/env python3
"""Opt-in actual uob/uob-sim OCPP 1.6J security event and ExtendedTriggerMessage proof, loopback only.

Flow: two independent simulator stations connect to one daemon. alpha raises three security events
(a standard one, a vendor one and one whose techInfo carries a credential) and resends each one
after its acknowledgement (duplicate fault), so the bridge must journal exactly three records, with
the credential redacted. alpha then answers ExtendedTriggerMessage for Heartbeat and
LogStatusNotification (observed) and rejects StatusNotification (unsupported). The bridge refuses
certificate, connector 0, extra-field, wrong-schema, plain-TriggerMessage, missing-capability and
unprivileged requests before anything reaches the station. bravo accepts but omits the triggered
message, which the bridge must end as absent at its deadline (about 60 to 90 s). Logs and SQLite
files must hold neither the station secret nor the event credential.

BRIDGE_INTERFACE pins the bridge-facing names this script relies on.
"""
import argparse
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import time
import urllib.error
import urllib.request

# ==================================== BRIDGE_INTERFACE ====================================
STATION_OPTIONS = ["extended_trigger_message", "get_log", "trigger_message"]  # privileged options + log capability
EXTENDED_SCHEMA = "urn:OCPP:Cp:1.6:2020:3:ExtendedTriggerMessage.req"
EVENT_TYPE = "station.security_event.16"  # journal event type of an accepted event
EVENT_PAYLOAD = "security_event_16"  # key of the event payload: event_type, standard, timestamp,
#                                      tech_info, tech_info_redactions
REVISION = {"LogStatusNotification": 19, "Heartbeat": 2, "StatusNotification": 2}  # command-result revision
CAPABILITY_STATUS, CAPABILITY_CODE = 422, "unsupported_operation"  # SignedUpdateFirmware is not advertised
DEADLINE_SECONDS = 60  # trigger observation window
# ==========================================================================================

CREDENTIAL = "hunter2-" + secrets.token_hex(4)  # carried in techInfo; must never be stored raw
BOOT = "{chargePointVendor='UOB',chargePointModel='Security16St',firmwareVersion='sim-1.0.0'}"
EVENTS = [("StartupOfTheDevice", "2026-10-01T12:00:00Z", None),
          ("VendorXyzEvent", "2026-10-01T12:00:01Z", None),
          ("TamperDetectionActivated", "2026-10-01T12:00:02+00:00", f"password={CREDENTIAL} mode=maintenance")]
STATIONS = {
    "alpha": "duplicate_acknowledged=true\n[stations.security16.extended_trigger]\nstatus_notification='rejected'\n",
    "bravo": "[stations.security16.extended_trigger]\nomit=true\n",
}


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


def wait_for(predicate, label, seconds=30):
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


def step(name, step_id, action, fields="", timeout=90000):
    return f"[[steps]]\nid='{step_id}'\nstation='{name}'\naction='{action}'\ntimeout_ms={timeout}\n{fields}\n"


def scenario(name):
    steps = step(name, "connect", "connect", timeout=5000)
    steps += step(name, "boot", "boot", f"fixture_id='wire.ocpp16.boot.valid'\npayload={BOOT}", 5000)
    if name == "alpha":
        for index, (kind, at, info) in enumerate(EVENTS):
            extra = f",techInfo='{info}'" if info else ""
            steps += step(name, f"event-{index}", "emit_security_event",
                          f"payload={{type='{kind}',timestamp='{at}'{extra}}}", 5000)
        steps += step(name, "acknowledged", "await_security",
                      "expect_response={pendingEvents=0,eventsEmitted=3,eventsAcknowledged=6,duplicatesSent=3}")
    # Hold the connection while the smoke sends ExtendedTriggerMessage commands.
    steps += step(name, "hold", "wait", "duration_ms=150000", 155000)
    return f"schema_version=1\nseed=127\n{steps}"


class Smoke:
    def __init__(self, bridge, simulator, output):
        self.bridge, self.simulator, self.output = bridge, simulator, output
        self.children, self.logs, self.checks = [], [], []
        self.management, self.charging, self.artifacts = port(), port(), port()
        require(len({self.management, self.charging, self.artifacts}) == 3, "distinct ports")
        self.grants = {name: "uob1.demo." + secrets.token_hex(16) for name in ["read", "control", "privileged"]}
        for name, grant in self.grants.items():
            private(output / name, grant)
        self.secret = "[REDACTED]" + secrets.token_hex(8)
        for name in STATIONS:  # one credential file per station
            private(output / f"station-{name}-auth", self.secret + "-" + name)
        (output / "spool").mkdir(mode=0o700)
        (output / "service-state").mkdir(mode=0o700)
        options = "".join(f"{option}=true\n" for option in STATION_OPTIONS)
        stations = "".join(f"""[[charging.stations]]
id='{name}'
protocol='ocpp16j'
credential_file='{output / f'station-{name}-auth'}'
{options}diagnostics_job_timeout_seconds=600
[[charging.stations.resources]]
connector_id='one'
native_connector_id=1
""" for name in STATIONS)
        private(output / "bridge.toml", f"""[bridge]
id='joint-127'
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
        environment = {key: value for key, value in os.environ.items() if key not in ["NOTIFY_SOCKET", "WATCHDOG_USEC"]}
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
        # Station-level keys precede tables, so the security16 keys sit in the table itself.
        security = STATIONS[name]
        table, _, extended = security.partition("[stations.security16.extended_trigger]")
        config = private(self.output / f"sim-{name}.toml", f"""schema_version=1
[[stations]]
id='{name}'
endpoint='ws://127.0.0.1:{self.charging}/ocpp/{name}'
ocpp_version='1.6'
credentials_file='{self.output / f'station-{name}-auth'}'
connectors=[1]
request_timeout_ms=10000
reconnect=true
[stations.security16]
private_state_file='{self.output / f'sim-{name}-security.json'}'
{table}
[stations.security16.extended_trigger]{extended}
[stations.diagnostics16]
private_state_file='{self.output / f'sim-{name}-diagnostics.json'}'
legacy=false
security_log=true
""")
        path = private(self.output / f"scenario-{name}.toml", scenario(name))
        child = self.start(self.simulator, ["run", "--config", str(config), "--scenario", str(path), "--format", "jsonl"],
                           "simulator-" + name)
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
        return status == 200 and any(value.get("point_id") == "ocpp16/registration/status"
                                     and value.get("value", {}).get("value") == "Accepted"
                                     for value in snapshot.get("current_values", []))

    def events(self, name, wanted, seconds=10):
        """Journal events of EVENT_TYPE for `name`, read from the server-sent stream (late
        subscribers replay retained events) until `wanted` arrived or the period ended."""
        request = urllib.request.Request(
            f"http://127.0.0.1:{self.management}/api/v1/events?station_id={name}&types={EVENT_TYPE}",
            headers={"Authorization": "Bearer " + self.grants["read"]})
        found, deadline = [], time.monotonic() + seconds
        with urllib.request.urlopen(request, timeout=2) as response:
            require(response.status == 200, f"events {response.status}")
            while len(found) < wanted and time.monotonic() < deadline:
                try:
                    line = response.readline()
                except OSError:  # an idle stream ends the read; a timed out chunked body is unusable
                    break
                require(line, "event stream ended")
                if line.startswith(b"data: "):
                    found.append(json.loads(line[6:]))
        return found

    def command(self, request_id, name, action, payload, schema, connector=False, grant="privileged"):
        resource = {"bridge_id": "joint-127", "station_id": name}
        if connector:
            resource = {**resource, "resource": {"kind": "connector", "connector_id": "one"},
                        "native_protocol_reference": {"protocol": "ocpp16", "connector_id": 1}}
        body = {"request_id": request_id, "resource": resource,
                "operation": {"kind": "ocpp", "parameters": {"protocol": "ocpp16j", "action": action,
                              "payload_schema": schema, "payload": payload}},
                "expires_at": "2099-01-01T00:00:00Z"}
        return self.http("/api/v1/commands", grant, body)

    def trigger(self, request_id, name, requested, connector_id=None, native="Accepted"):
        payload = {"requestedMessage": requested}
        if connector_id is not None:
            payload["connectorId"] = connector_id
        status, response = self.command(request_id, name, "ExtendedTriggerMessage", payload, EXTENDED_SCHEMA,
                                        connector_id is not None)
        require(status == 202, f"{request_id} admission: {status} {json.dumps(response)}")
        result = response["result"]
        require(result["schema_version"]["revision"] == REVISION[requested], f"{request_id} revision")
        require(result["trigger_observation"]["native_response"] == native, f"{request_id} native reply")
        self.private_result(result)
        return result

    def refuse(self, request_id, name, action, payload, schema, status=400, code="command.invalid_schema_or_payload",
               grant="privileged"):
        """A command the bridge refuses before anything reaches the charger."""
        body = self.command(request_id, name, action, payload, schema, grant=grant)
        require(body[0] == status, f"{request_id} must be refused with {status}: {body[0]} {json.dumps(body[1])}")
        require(code in json.dumps(body[1]), f"{request_id} must name {code}: {json.dumps(body[1])}")
        self.private_result(body[1])
        return body[1]

    def observation(self, request_id):
        status, result = self.http("/api/v1/commands/" + request_id, "read")
        return result["trigger_observation"] if status == 200 and "trigger_observation" in result else None

    def reach(self, request_id, state, seconds=30):
        wait_for(lambda: (self.observation(request_id) or {}).get("status") == state,
                 f"{request_id} did not reach {state}: {self.observation(request_id)}", seconds)
        self.checks.append(f"{request_id} observation reached {state}")
        return self.observation(request_id)

    def private_result(self, value):
        text = json.dumps(value)
        for marker in [self.secret, CREDENTIAL, *self.grants.values()]:
            require(marker not in text, "private material in public result")

    def check_events(self):
        journal = self.events("alpha", 4)  # a fourth record would be a failed deduplication
        require(len(journal) == 3, f"three journal records after six sends: {len(journal)}")
        require(all(event["event_type"] == EVENT_TYPE for event in journal), "journal event type")
        records = {record["event_type"]: record for record in (event["payload"][EVENT_PAYLOAD] for event in journal)}
        require(set(records) == {kind for kind, _, _ in EVENTS}, f"event types {sorted(records)}")
        require(records["StartupOfTheDevice"]["standard"] is True, "standard event flag")
        require(records["VendorXyzEvent"]["standard"] is False, "vendor event kept verbatim, not standard")
        tamper = records["TamperDetectionActivated"]
        require("[redacted]" in tamper["tech_info"] and "mode=maintenance" in tamper["tech_info"], "context kept")
        require(tamper["tech_info_redactions"] >= 1, "redaction counted")
        require("2026-10-01T12:00:02" in json.dumps(tamper), "timestamp normalised to UTC")
        text = json.dumps(journal)
        require(CREDENTIAL not in text, "credential in the event stream")
        self.private_result(journal)
        # A late subscriber replays the identical records.
        require(self.events("alpha", 3) == journal, "late subscriber replays identical records")
        self.checks.append("six sends journaled as three records, credential redacted, replay identical")

    def run(self):
        self.daemon("daemon-1")
        alpha, bravo = self.station("alpha"), self.station("bravo")
        wait_for(lambda: len(self.events("alpha", 3, 2)) == 3, "alpha events journaled", 60)
        self.check_events()
        self.trigger("heartbeat-1", "alpha", "Heartbeat")
        self.reach("heartbeat-1", "observed")
        self.trigger("log-1", "alpha", "LogStatusNotification")
        self.reach("log-1", "observed")  # an Idle status without requestId is still evidence
        self.trigger("status-1", "alpha", "StatusNotification", 1, native="Rejected")
        self.reach("status-1", "unsupported")
        self.checks.append("Heartbeat and LogStatusNotification observed, rejected StatusNotification unsupported")
        extended = "ExtendedTriggerMessage"
        # The offered descriptor lacks these values, so the bridge refuses them before the adapter.
        self.refuse("sign-1", "alpha", extended, {"requestedMessage": "SignChargePointCertificate"}, EXTENDED_SCHEMA)
        self.refuse("zero-1", "alpha", extended, {"requestedMessage": "MeterValues", "connectorId": 0}, EXTENDED_SCHEMA)
        self.refuse("extra-1", "alpha", extended, {"requestedMessage": "Heartbeat", "note": 1}, EXTENDED_SCHEMA)
        self.refuse("urn-1", "alpha", extended, {"requestedMessage": "Heartbeat"}, "urn:OCPP:1.6:2019:12:TriggerMessageRequest")
        self.refuse("plain-1", "alpha", "TriggerMessage", {"requestedMessage": "LogStatusNotification"},
                    "urn:OCPP:1.6:2019:12:TriggerMessageRequest")
        self.refuse("signed-1", "alpha", extended, {"requestedMessage": "FirmwareStatusNotification"}, EXTENDED_SCHEMA,
                    CAPABILITY_STATUS, CAPABILITY_CODE)
        self.refuse("control-1", "alpha", extended, {"requestedMessage": "Heartbeat"}, EXTENDED_SCHEMA, 403, "", "control")
        self.checks.append("certificate, connector 0, extra field, wrong schema, plain class, missing capability and "
                           "unprivileged requests refused")
        self.trigger("omitted-1", "bravo", "Heartbeat")
        # bravo accepted but sent nothing; the bridge ends the window as absent.
        self.reach("omitted-1", "absent", DEADLINE_SECONDS + 30)
        for child in (alpha, bravo):
            child.terminate()
            child.wait(timeout=20)
        for path in self.output.glob("*.std*"):
            require(self.secret.encode() not in path.read_bytes(), "station secret in logs")
            require(CREDENTIAL.encode() not in path.read_bytes(), "event credential in logs")
        for path in (self.output / "service-state").iterdir():
            require(CREDENTIAL.encode() not in path.read_bytes(), f"event credential in {path.name}")
        self.checks.append("no credential in any log or SQLite file")
        private(self.output / "smoke.json", json.dumps({"checks": self.checks}, indent=2) + "\n")
        print(f"1.6 security joint smoke passed {len(self.checks)} checks")

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
