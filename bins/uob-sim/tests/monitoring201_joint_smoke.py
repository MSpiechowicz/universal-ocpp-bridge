#!/usr/bin/env python3
"""Opt-in actual uob/uob-sim OCPP 2.0.1 monitoring proof, synthetic loopback only.

Flow: two independent simulator stations connect to one daemon. alpha has monitors set (the
bridge lists them in a multi-fragment report), an event burst acknowledged, an invalid monitor
refused, one monitor cleared and the next report no longer listing it. bravo closes its socket in
the middle of a report, which the bridge must end as Incomplete.

The bridge interface (privileged station options, payload schema URNs, `monitoring_201` evidence and
`station.monitoring_event.201` journal events) is pinned in the BRIDGE_INTERFACE block below.
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
# Station options that enable the five privileged monitoring commands; get_variables lets the
# bridge learn the MonitoringCtrlr message limits that multi-item Set/Clear requests need.
STATION_OPTIONS = ["get_variables", "set_variable_monitoring", "clear_variable_monitoring",
                   "get_monitoring_report", "set_monitoring_base", "set_monitoring_level"]
EXPECTED_REVISION = 18  # command-result schema revision (V1_MONITORING_201)
EVIDENCE = "monitoring_201"  # command result key holding {"query": ..., "outcome": ...}
EVENT_TYPE = "station.monitoring_event.201"  # journal event type of an accepted NotifyEvent


def payload_schema(action):
    return f"urn:OCPP:Cp:2:2020:3:{action}Request"


def statuses(evidence):
    """Per-item statuses of a Set/Clear outcome, in request order."""
    return [entry["status"] for entry in evidence["outcome"]["monitors"]]
# ==========================================================================================

BRAVO_REQUEST = 902  # the interrupted report's requestId, known to the bravo scenario
BOOT = "{reason='PowerUp',chargingStation={vendorName='UOB',model='MonitoringSt201',firmwareVersion='sim-1.0.0'}}"
CONNECTOR = "{name='Connector',evse={id=1,connectorId=1}}"
# Independent station model: same seed on every station (see docs/simulator/monitoring201.md).
SEED = """[stations.monitoring201]
{fields}
[[stations.monitoring201.variables]]
component = { name = "Connector", evse = { id = 1, connectorId = 1 } }
variable = { name = "Temperature" }
value = "40"
min = -40.0
max = 150.0
monitor_types = ["UpperThreshold", "LowerThreshold", "Delta"]
[[stations.monitoring201.variables]]
component = { name = "ChargingStation" }
variable = { name = "SupplyVoltage" }
value = "230"
min = 0.0
max = 300.0
monitor_types = ["UpperThreshold", "LowerThreshold", "Delta"]
[[stations.monitoring201.variables]]
component = { name = "TemperatureSensor", instance = "Cabinet" }
variable = { name = "Temperature" }
value = "30"
min = -40.0
max = 120.0
monitor_types = ["UpperThreshold", "Delta", "Periodic", "PeriodicClockAligned"]
[[stations.monitoring201.monitors]]
id = 4003
kind = "hard_wired"
component = { name = "ChargingStation" }
variable = { name = "SupplyVoltage" }
type = "UpperThreshold"
value = 280.0
severity = 1
[[stations.monitoring201.monitors]]
id = 4030
kind = "preconfigured"
component = { name = "TemperatureSensor", instance = "Cabinet" }
variable = { name = "Temperature" }
type = "UpperThreshold"
value = 100.0
severity = 4
"""
# alpha: three fragments for five monitors; bravo: one monitor per fragment, socket closed after one.
STATIONS = {
    "alpha": "monitors_per_fragment = 2\nevents_per_message = 2",
    "bravo": "monitors_per_fragment = 1\nreport_fault = \"disconnect_mid_report\"",
}
ALPHA_MONITORS = [4003, 4030, 4031, 4032, 4033]


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


def scenario(name, steps):
    return f"""schema_version=1
seed=126
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
payload={BOOT}
{steps}"""


def step(name, step_id, action, fields, timeout=90000):
    return f"[[steps]]\nid='{step_id}'\nstation='{name}'\naction='{action}'\ntimeout_ms={timeout}\n{fields}\n"


def alpha_steps():
    awaiting = lambda sid, expect: step("alpha", sid, "await_monitoring", f"expect_response={{{expect}}}")
    ids = ",".join(map(str, ALPHA_MONITORS))
    return (awaiting("monitors-set", f"monitorIds=[{ids}]")
            + awaiting("report-delivered", "reportsDelivered=1,reportFragmentsSent=3")
            # 95 exceeds the CSMS's 75 UpperThreshold: one Alerting event, then a burst of five
            # events chained over three NotifyEvent messages: four acknowledged calls.
            + step("alpha", "exceed", "set_monitored_value", f"payload={{component={CONNECTOR},variable={{name='Temperature'}},value='95'}}")
            + step("alpha", "burst", "emit_monitoring_events", f"payload={{component={CONNECTOR},variable={{name='Temperature'}},count=5,monitorId=4031}}")
            + awaiting("events-acknowledged", "eventsAcknowledged=4,eventsRejected=0,eventsQueued=0,lastEventOutcome='Result'")
            # the disconnect must wait for the CSMS's last command, the level change
            + awaiting("monitor-cleared", "monitorIds=[4003,4030,4032,4033],level=7")
            + step("alpha", "final", "assert_monitoring", "expect_response={monitorCount=4,hardWired=1}")
            + step("alpha", "disconnect", "disconnect", "", 5000))


def bravo_steps():
    return step("bravo", "interrupted", "await_monitoring",
                f"expect_response={{lastReport={{requestId={BRAVO_REQUEST},planned=2,sent=1,outcome='Interrupted'}}}}")


class Smoke:
    def __init__(self, bridge, simulator, output):
        self.bridge, self.simulator, self.output = bridge, simulator, output
        self.children, self.logs, self.checks = [], [], []
        self.management, self.charging = port(), port()
        require(self.management != self.charging, "distinct ports")
        self.grants = {name: "uob1.demo." + secrets.token_hex(16) for name in ["read", "control", "privileged"]}
        for name, grant in self.grants.items():
            private(output / name, grant)
        self.secret = "station-secret-" + secrets.token_hex(8)
        for name in STATIONS:  # one credential file per station
            private(output / f"station-{name}-auth", self.secret + "-" + name)
        (output / "service-state").mkdir(mode=0o700)
        options = "".join(f"{option}=true\n" for option in STATION_OPTIONS)
        stations = "".join(f"""[[charging.stations]]
id='{name}'
protocol='ocpp201'
credential_file='{output / f'station-{name}-auth'}'
{options}[[charging.stations.resources]]
evse_id='one'
native_evse_id=1
[[charging.stations.resources]]
evse_id='one'
native_evse_id=1
connector_id='one'
native_connector_id=1
""" for name in STATIONS)
        private(output / "bridge.toml", f"""[bridge]
id='joint-126'
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

    def daemon(self):
        child = self.start(self.bridge, ["serve", "--config", str(self.output / "bridge.toml"), "--no-ui"], "daemon")
        wait_for(lambda: child.poll() is None and listening(self.management), "actual daemon readiness")
        return child

    def station(self, name, steps):
        config = private(self.output / f"sim-{name}.toml", f"""schema_version=1
[[stations]]
id='{name}'
endpoint='ws://127.0.0.1:{self.charging}/ocpp/{name}'
ocpp_version='2.0.1'
credentials_file='{self.output / f'station-{name}-auth'}'
request_timeout_ms=10000
reconnect=false
[[stations.evses]]
id=1
connectors=[1]
""" + SEED.replace("{fields}", STATIONS[name]))
        path = private(self.output / f"scenario-{name}.toml", scenario(name, steps))
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
        return status == 200 and any(value.get("point_id") == "ocpp201/registration/status"
                                     and value.get("value", {}).get("value") == "Accepted"
                                     for value in snapshot.get("current_values", []))

    def command(self, request_id, name, action, payload):
        body = {"request_id": request_id, "resource": {"bridge_id": "joint-126", "station_id": name},
                "operation": {"kind": "ocpp", "parameters": {"protocol": "ocpp201", "action": action,
                              "payload_schema": payload_schema(action), "payload": payload}},
                "expires_at": "2099-01-01T00:00:00Z"}
        return self.http("/api/v1/commands", body=body)

    def submit(self, request_id, name, action, payload, accepted=True):
        """A command the station answered; `accepted` is the bridge's protocol-level verdict."""
        status, response = self.command(request_id, name, action, payload)
        require(status == 202, f"{request_id} admission: {status} {json.dumps(response)}")
        result = response["result"]
        require(result["lifecycle"]["stage"] == "protocol_response", f"{request_id} stage {result['lifecycle']}")
        require(result["lifecycle"]["accepted"] is accepted, f"{request_id} acceptance {result['lifecycle']}")
        require(result["schema_version"]["revision"] == EXPECTED_REVISION, "command-result revision")
        require(EVIDENCE in result, f"{request_id} carries {EVIDENCE}")
        require(result[EVIDENCE]["query"]["action"] == action, f"{request_id} query action")
        self.private_result(result)
        return result[EVIDENCE]

    def refuse(self, request_id, name, action, payload, code="command.invalid_schema_or_payload"):
        """A command the bridge refuses before anything reaches the charger."""
        status, response = self.command(request_id, name, action, payload)
        require(status == 400, f"{request_id} must be refused: {status} {json.dumps(response)}")
        refusal = response.get("error") or (response.get("lifecycle", {}).get("error") or {}).get("code")
        require(refusal == code, f"{request_id} refusal {code} expected: {json.dumps(response)}")
        self.private_result(response)
        return response

    def private_result(self, value):
        text = json.dumps(value)
        for marker in [self.secret, *self.grants.values()]:
            require(marker not in text, "private material in public result")

    def report(self, request_id):
        status, result = self.http("/api/v1/commands/" + request_id, "read")
        return result[EVIDENCE]["outcome"].get("report") if status == 200 and EVIDENCE in result else None

    def reach_report(self, request_id, state, seconds=60):
        wait_for(lambda: (self.report(request_id) or {}).get("state") == state,
                 f"{request_id} did not reach {state}: {self.report(request_id)}", seconds)
        self.checks.append(f"{request_id} report reached {state}")
        return self.report(request_id)

    def events(self, name, wanted, seconds=30):
        """Journal events of EVENT_TYPE for `name`, read from the server-sent stream (late
        subscribers replay retained events) until `wanted` arrived."""
        request = urllib.request.Request(
            f"http://127.0.0.1:{self.management}/api/v1/events?station_id={name}&types={EVENT_TYPE}",
            headers={"Authorization": "Bearer " + self.grants["read"]})
        found, deadline = [], time.monotonic() + seconds
        with urllib.request.urlopen(request, timeout=2) as response:
            require(response.status == 200, f"events {response.status}")
            while len(found) < wanted and time.monotonic() < deadline:
                try:
                    line = response.readline()
                except (socket.timeout, TimeoutError):
                    continue
                require(line, "event stream ended")
                if line.startswith(b"data: "):
                    found.append(json.loads(line[6:]))
        return found

    def learn_limits(self, name):
        entries = [{"component": {"name": "MonitoringCtrlr"}, "variable": {"name": variable, "instance": instance}}
                   for instance in ["SetVariableMonitoring", "ClearVariableMonitoring"]
                   for variable in ["ItemsPerMessage", "BytesPerMessage"]]
        # One variable per GetVariables: the bridge sends several only within learned limits.
        for index, entry in enumerate(entries):
            status, response = self.command(f"limits-{name}-{index}", name, "GetVariables", {"getVariableData": [entry]})
            require(status == 202 and response["result"]["lifecycle"]["accepted"] is True, f"limits {status} {response}")

    def run(self):
        self.daemon()
        alpha = self.station("alpha", alpha_steps())
        bravo = self.station("bravo", bravo_steps())
        temperature = {"name": "Temperature"}
        connector = {"name": "Connector", "evse": {"id": 1, "connectorId": 1}}
        cabinet = {"name": "TemperatureSensor", "instance": "Cabinet"}
        three = [
            {"value": 75.0, "type": "UpperThreshold", "severity": 3, "component": connector, "variable": temperature},
            {"value": 207.0, "type": "LowerThreshold", "severity": 4, "component": {"name": "ChargingStation"}, "variable": {"name": "SupplyVoltage"}},
            {"value": 300, "type": "Periodic", "severity": 9, "component": cabinet, "variable": temperature},
        ]
        # Without learned limits the bridge transmits only single-item Set requests.
        self.refuse("unlearned", "alpha", "SetVariableMonitoring", {"setMonitoringData": three}, "policy_rejected")
        self.checks.append("multi-item Set refused before the limits are learned")
        self.learn_limits("alpha")
        self.checks.append("MonitoringCtrlr limits learned through GetVariables")
        evidence = self.submit("set-1", "alpha", "SetVariableMonitoring", {"setMonitoringData": three})
        require(evidence["outcome"]["kind"] == "set_monitors", "set outcome kind")
        require(statuses(evidence) == ["Accepted"] * 3, f"three monitors accepted: {evidence['outcome']}")
        require([m["id"] for m in evidence["outcome"]["monitors"]] == [4031, 4032, 4033], "generated ids")
        self.checks.append("set-1 accepted three monitors with per-item ids")
        evidence = self.submit("report-1", "alpha", "GetMonitoringReport", {"requestId": 801})
        require(evidence["outcome"]["ack"] == "Accepted", "report accepted")
        state = self.reach_report("report-1", "complete")
        listed = sorted(monitor["id"] for monitor in state["monitors"])
        require(len(state["fragments"]) == 3 and listed == ALPHA_MONITORS,
                f"multi-fragment report listed all monitors: {state}")
        self.checks.append("multi-fragment report lists all five monitors")
        # The station's own scenario now raises the events; it passes only if the bridge answered
        # every NotifyEvent with an empty NotifyEventResponse (1 alert + 3 burst messages).
        journal = self.events("alpha", 4)
        require(len(journal) == 4, f"four journal records: {len(journal)}")
        records = [event["payload"]["monitoring_events_201"] for event in journal]
        require(all(event["event_type"] == EVENT_TYPE for event in journal), "journal event type")
        require([r["seq_no"] for r in records] == [0, 0, 1, 2], f"sequence numbers {[r['seq_no'] for r in records]}")
        require(all(r["ordering"]["kind"] in ("in_sequence", "restarted") for r in records), "ordering")
        require(records[0]["events"][0]["trigger"] == "Alerting" and records[0]["events"][0]["actual_value"] == "95",
                f"alert event {records[0]['events']}")
        require(sum(len(r["events"]) for r in records[1:]) == 5, "burst events kept")
        self.checks.append("event burst acknowledged and journaled as four station.monitoring_event.201 records")
        # Refused by the bridge: nothing may reach the charger (the scenario would see a monitor).
        negative = {"value": -1.0, "type": "Delta", "severity": 3, "component": connector, "variable": temperature}
        severity = dict(negative, value=1.0, severity=10)
        self.refuse("invalid-delta", "alpha", "SetVariableMonitoring", {"setMonitoringData": [negative]})
        self.refuse("invalid-severity", "alpha", "SetVariableMonitoring", {"setMonitoringData": [severity]})
        self.checks.append("negative Delta and severity 10 refused by the bridge (HTTP 400)")
        # Refused by the charger: the bridge keeps each native per-item result.
        unknown = {"value": 1.0, "type": "Delta", "severity": 3, "component": {"name": "Heater"}, "variable": temperature}
        evidence = self.submit("unknown-1", "alpha", "SetVariableMonitoring", {"setMonitoringData": [unknown]}, accepted=False)
        require(statuses(evidence) == ["UnknownComponent"], "charger refusal surfaced per item")
        self.checks.append("charger-side UnknownComponent surfaced per item")
        evidence = self.submit("clear-1", "alpha", "ClearVariableMonitoring", {"id": [4031, 4999, 4003]}, accepted=False)
        require(evidence["outcome"]["kind"] == "clear_monitors", "clear outcome kind")
        require(statuses(evidence) == ["Accepted", "NotFound", "Rejected"], f"clear statuses {evidence['outcome']}")
        require(evidence["outcome"]["monitors"][2]["reason_code"] == "HardWired", "hard-wired reason")
        self.checks.append("clear-1 removed one monitor, NotFound and the hard-wired refusal per item")
        evidence = self.submit("report-2", "alpha", "GetMonitoringReport", {
            "requestId": 802, "componentVariable": [{"component": connector}]}, accepted=False)
        require(evidence["outcome"]["ack"] == "EmptyResultSet", "cleared monitor no longer listed")
        state = self.report("report-2")
        require(state["state"] == "not_expected", f"EmptyResultSet expects no report: {state}")
        self.checks.append("report-2 for the cleared component is EmptyResultSet / not_expected")
        evidence = self.submit("base-1", "alpha", "SetMonitoringBase", {"monitoringBase": "All"})
        require(evidence["outcome"]["kind"] == "base" and evidence["outcome"]["status"] == "Accepted", "base")
        evidence = self.submit("level-1", "alpha", "SetMonitoringLevel", {"severity": 7})
        require(evidence["outcome"]["kind"] == "level" and evidence["outcome"]["status"] == "Accepted", "level")
        self.checks.append("SetMonitoringBase and SetMonitoringLevel carry the native status")
        self.submit("interrupted-1", "bravo", "GetMonitoringReport", {"requestId": BRAVO_REQUEST})
        state = self.reach_report("interrupted-1", "incomplete")
        require(state.get("reason"), "an interrupted report names why it is incomplete")
        self.checks.append(f"interrupted report ended incomplete: {state['reason']}")
        require(alpha.wait(timeout=90) == 0, "simulator alpha scenario completed")
        require(bravo.wait(timeout=90) == 0, "simulator bravo scenario completed")
        self.checks.append("independent simulator scenarios passed")
        for name in STATIONS:
            status, history = self.http("/api/v1/commands?station_id=" + name, "read")
            require(status == 200, "history")
            self.private_result(history)
        for path in self.output.glob("*.std*"):
            require(self.secret.encode() not in path.read_bytes(), "station secret in logs")
        private(self.output / "smoke.json", json.dumps({"checks": self.checks}, indent=2) + "\n")
        print(f"2.0.1 monitoring joint smoke passed {len(self.checks)} checks")

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
