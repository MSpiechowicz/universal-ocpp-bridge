"""Independent native scenarios and real durable/public reservation acceptance phases."""
import json
import threading
import time
from datetime import datetime
import secrets
import sqlite3


def same_instant(left, right):
    return (isinstance(left, str) and isinstance(right, str)
            and datetime.fromisoformat(left.replace("Z", "+00:00"))
            == datetime.fromisoformat(right.replace("Z", "+00:00")))


def toml_value(value):
    if isinstance(value, dict):
        return "{" + ",".join(json.dumps(key) + "=" + toml_value(item) for key, item in value.items()) + "}"
    if isinstance(value, list):
        return "[" + ",".join(toml_value(item) for item in value) + "]"
    return json.dumps(value, ensure_ascii=False)


def scenario_document(steps):
    return "schema_version=1\nseed=113\n" + "".join(
        "\n[[steps]]\n" + "\n".join(key + "=" + toml_value(value) for key, value in entry.items()) + "\n"
        for entry in steps)


def step(name, action, **fields):
    return {"id": name, "station": "alpha", "action": action, "timeout_ms": 10000, **fields}


def boot():
    return step("boot", "boot", fixture_id="wire.ocpp16.boot.valid",
                payload={"chargePointVendor": "UOB", "chargePointModel": "Reservations"})


def parked():
    return step("park", "wait", duration_ms=60000, timeout_ms=65000)


def available(connector):
    return step("available-" + str(connector), "status", fixture_id="wire.ocpp16.availability-status",
                payload={"connectorId": connector, "status": "Available", "errorCode": "NoError"})


def status(connector, value):
    return step(value.lower() + str(connector), "status", fixture_id="wire.ocpp16.availability-status",
                payload={"connectorId": connector, "status": value, "errorCode": "NoError"})


def authorize(name, token):
    return step(name, "authorize", fixture_id="wire.ocpp16.authorization.valid", payload={"idTag": token})


def gate(count=1):
    return step("reservation-gate", "await_reservation", expect_response={"activeReservations": count})


def start(name, connector, token, timestamp=None, failure=False, current=False):
    payload = {"connectorId": connector, "idTag": token, "meterStart": 113}
    if not current:
        payload["timestamp"] = timestamp
    value = step(name, "start_transaction", fixture_id="wire.ocpp16.transaction-start.valid",
                 payload=payload)
    if current:
        value["use_current_timestamp"] = True
    if failure:
        value["expect_failure"] = "charging_call_failed"
    return value


def stop(name):
    # Frees the connector: the service keeps at most one open transaction per connector.
    return step(name, "stop_transaction", fixture_id="wire.ocpp16.transaction-stop.valid",
                payload={"meterStop": 114, "reason": "Local"}, use_current_timestamp=True,
                use_active_transaction=True)


def native_status(h, connector, value, wait_for):
    wait_for(lambda: h.native()["connectors"].get(str(connector)) == value,
             "actual native connector state missing")


def accepted(result, require):
    require(result["reservation_16"]["status"] == "Accepted", "actual Accepted native status missing")


def assert_current_source(h, identifier, require):
    starts = [call for call in h.relay.calls if call["direction"] == "station"
              and call["action"] == "StartTransaction" and call.get("reservationId") == identifier]
    require(bool(starts), "genuine current-source StartTransaction missing")
    record = [row for row in h.records() if row["reservation_id"] == identifier][-1]
    source = starts[-1]["timestamp"]
    source_time = datetime.fromisoformat(source.replace("Z", "+00:00"))
    admission_time = datetime.fromisoformat(record["admitted_at"].replace("Z", "+00:00"))
    require(source_time >= admission_time,
            "native source timestamp preceded latest reused-ID admission")
    require(same_instant(source, record.get("source_time")),
            "durable source evidence differs from actual native source timestamp")


def run_flow(h, daemon, require, wait_for, private, date):
    h.station([available(1), available(2)])
    native_status(h, 1, "Available", wait_for)
    h.command("nonprivileged-reserve", grant="read")
    h.command("raw-public-token", override={"idTag": h.owner}, deny=True)
    h.command("immutable-metadata-mismatch", override={"reservationId": 999}, deny=True)
    h.command("signed-id-overflow", override={"reservationId": 2147483648}, deny=True)
    h.command("raw-public-parent", override={"parentIdTag": h.group}, deny=True)
    h.command("negative-connector", override={"connectorId": -1}, deny=True)
    h.command("positive-scope-mismatch", override={"connectorId": 2}, deny=True)
    h.command("expired-public-deadline", deadline=date(-1), deny=True)
    h.command("wrong-station", scope={"station_id": "not-alpha"}, deny=True)
    h.command("child-scoped-cancel-forbidden", "CancelReservation", identifier=-113,
              scope={"resource": {"kind": "connector", "connector_id": "one"},
                     "native_protocol_reference": {"protocol": "ocpp16", "connector_id": 1}}, deny=True)
    accepted(h.command("expiry-native", request="expiry"), require)
    h.workflow(115, "active")
    h.workflow(115, "expired")
    wait_for(lambda: not h.native()["reservations"], "native idle expiry did not persist")
    require(any(call["direction"] == "station" and call["action"] == "StatusNotification"
                and call.get("status") == "Available" for call in h.relay.calls), "native expiry status missing")

    accepted(h.command("negative-exact", request="exact"), require)
    h.workflow(-113, "active")
    require(h.command("occupied-native", request="occupied")["reservation_16"]["status"] == "Occupied", "native Occupied lost")
    accepted(h.command("same-id-replacement", request="replacement"), require)
    rows = h.records()
    require([r for r in rows if r["reservation_id"] == -113][-1]["candidate"]["connector_id"] == 2,
            "replacement silently retained old connector")
    accepted(h.command("station-cancel", "CancelReservation", identifier=-113), require)
    h.workflow(-113, "cancelled")
    require(h.command("cancel-absent", "CancelReservation", identifier=-113)["reservation_16"]["status"] == "Rejected", "absent cancel invented acceptance")
    h.command("cancel-extra-native-field", "CancelReservation", identifier=-113, override={"connectorId": 1}, deny=True)

    for label, identifier, native in [("faulted", -2147483648, "Faulted"), ("unavailable", 2147483647, "Unavailable")]:
        h.station([status(1, native)])
        native_status(h, 1, native, wait_for)
        result = h.command(label + "-native", request=label)
        require(result["reservation_16"]["status"] == native, "native rejection status lost")
        h.workflow(identifier, "rejected")
    h.station([available(1), available(2)])
    native_status(h, 1, "Available", wait_for)
    accepted(h.command("fault-termination-create"), require)
    h.station([status(1, "Faulted")])
    h.workflow(-113, "faulted")
    h.station([available(1)])
    native_status(h, 1, "Available", wait_for)
    accepted(h.command("unavailable-termination-create"), require)
    h.station([status(1, "Unavailable")])
    h.workflow(-113, "unavailable")

    for label, replacement in [("zero-unsupported", "reserve_connector_zero_supported=false"), ("disabled-native", "enabled=false")]:
        source = (h.output / "sim.toml").read_text()
        original = "reserve_connector_zero_supported=true" if label == "zero-unsupported" else "enabled=true"
        path = private(h.output / (label + ".toml"), source.replace(original, replacement))
        h.station([available(1), available(2)], config=path.name)
        native_status(h, 1, "Available", wait_for)
        require(h.command(label, request="any")["reservation_16"]["status"] == "Rejected", "native support/policy not enforced")

    h.station([available(1), available(2), authorize("owner-auth", h.owner), authorize("member-auth", h.member),
               gate(), start("group-start", 1, h.member, date(-1)), stop("group-start-stop")])
    native_status(h, 1, "Available", wait_for)
    accepted(h.command("actual-parent-group", request="group"), require)
    h.workflow(113, "consumed")
    wait_for(lambda: not h.native()["reservations"], "native matching group did not consume")
    require(any(call["action"] == "StartTransaction" and call.get("reservationId") == 113 for call in h.relay.calls), "StartTransaction omitted actual reservationId")
    h.evidence["checks"].append("actual Authorize parent-to-parent group match and native termination")

    h.station([available(1), available(2), gate(), start("wrong-connector-start", 2, h.owner, date(-1)), stop("wrong-connector-start-stop")])
    native_status(h, 1, "Available", wait_for)
    accepted(h.command("wrong-connector-reserve"), require)
    wait_for(lambda: h.native()["connectors"]["2"] == "Occupied", "actual unreserved connector start missing")
    h.workflow(-113, "active")
    accepted(h.command("cleanup-wrong-connector", "CancelReservation", identifier=-113), require)

    h.station([available(1), available(2), authorize("any-other-policy", h.other), gate(), status(1, "Charging"),
               start("any-mismatch-capacity", 2, h.other, date(-1), failure=True)])
    native_status(h, 1, "Available", wait_for)
    accepted(h.command("zero-is-any", request="any"), require)
    wait_for(lambda: h.native()["connectors"]["1"] == "Occupied", "native occupancy did not gate Any")
    h.workflow(0, "active")
    require(h.native()["reservations"]["0"]["connectorId"] == 0, "Any incorrectly bound a concrete connector")
    accepted(h.command("cancel-any", "CancelReservation", identifier=0), require)
    h.station([available(1), available(2), gate(), start("matching-any-start", 2, h.owner, current=True), stop("matching-any-start-stop")])
    accepted(h.command("zero-unbound-match", request="any"), require)
    h.workflow(0, "consumed")
    assert_current_source(h, 0, require)

    h.station([available(1), available(2), authorize("other-identity", h.other), gate(),
               start("wrong-token-start", 1, h.other, date(-1), failure=True),
               {**start("parent-not-token-start", 1, h.group, date(-1)), "expect_failure": "not_authorized"}])
    accepted(h.command("wrong-token-and-parent-token"), require)
    h.workflow(-113, "active")
    time.sleep(0.2)
    require(h.sim.poll() is None, "native mismatch scenario failed before its final parked checkpoint")
    require(h.native()["reservations"].get("-113") is not None, "unrelated token/group-only identity consumed exact reservation")
    accepted(h.command("cleanup-mismatches", "CancelReservation", identifier=-113), require)

    h.station([available(1), available(2), authorize("cache-denied-before-revoke", h.denied)])
    native_status(h, 1, "Available", wait_for)
    wait_for(lambda: any(entry.get("idTag") == h.denied for entry in json.loads((h.output / "sim-identity.json").read_text())["cache"].values()), "actual native central cache population missing")
    h.sim.kill()
    h.sim.wait(timeout=10)
    daemon.kill()
    daemon.wait(timeout=10)
    next(identity for identity in h.identities if identity["idTag"] == h.denied).update(authorize=False, policy_revision=2)
    h.write_provisioning()
    daemon = h.daemon()
    timestamp = date(-2)
    # Blocked still opens a service transaction; connector 2 is not reused afterwards.
    h.station([available(1), available(2), gate(), start("real-denied-start", 2, h.denied, timestamp)])
    native_status(h, 2, "Available", wait_for)
    accepted(h.command("denied-real-create", request="denied"), require)
    h.workflow(114, "consumed", source=timestamp)
    with sqlite3.connect(f"file:{h.output / 'service-state/charging.sqlite3'}?mode=ro", uri=True) as connection:
        snapshots = [json.loads(row[0]) for row in connection.execute("SELECT payload FROM station_snapshots")]
    require(any(tx.get("ocpp16", {}).get("reservation_id") == 114 and tx.get("ocpp16", {}).get("authorization_status") == "Blocked"
                for snapshot in snapshots for tx in snapshot.get("transactions", [])), "denied reported transaction not retained honestly")
    h.evidence["checks"].append("trusted current policy denies real cached-native reported start without erasing reservation consumption")

    h.station([available(1), available(2)], pre=[step("delay-ack", "delay_local_reply", duration_ms=400)])
    native_status(h, 1, "Available", wait_for)
    before = len(h.relay.mutations())
    accepted(h.command("actual-delayed-ack", request="delayed"), require)
    observed = h.relay.mutations()[before:]
    require(len(observed) == 1 and observed[0].get("reply_elapsed_ms", 0) >= 350, "actual ACK was not delayed")
    h.workflow(116, "active")
    accepted(h.command("cancel-delayed", "CancelReservation", identifier=116), require)

    h.station([available(1), available(2)], pre=[step("drop-ack", "drop_local_reply")])
    native_status(h, 1, "Available", wait_for)
    before = len(h.relay.mutations())
    h.command("actual-dropped-ack", request="drop", uncertain=True)
    h.workflow(117, "uncertain")
    require([row for row in h.records() if row["reservation_id"] == 117][-1]["unresolved"],
            "lost ACK workflow uncertainty was not durably retained")
    require(h.native()["reservations"]["117"]["connectorId"] == 0, "lost ACK native mutation was not durable")
    require(len(h.relay.mutations()) == before + 1, "lost ACK mutation repeated")
    daemon.kill()
    daemon.wait(timeout=10)
    h.sim.kill()
    h.sim.wait(timeout=10)
    daemon = h.daemon()
    h.station()
    time.sleep(0.3)
    require(len(h.relay.mutations()) == before + 1, "daemon/simulator restart replayed native reservation")
    require(h.native()["reservations"]["117"]["connectorId"] == 0, "native killed-process recovery lost accepted mutation")
    h.workflow(117, "uncertain")
    accepted(h.command("explicit-cancel-uncertain", "CancelReservation", identifier=117), require)
    h.workflow(117, "cancelled")
    h.evidence["checks"].append("real lost-ACK mutation plus kill/new-process durable recovery without automatic replay")

    # Hold a genuine original StartTransaction at the relay, not a fabricated native call.
    # The CSMS expiry timer must use receipt time while retaining the station source timestamp.
    h.sim.kill()
    h.sim.wait(timeout=10)
    daemon.kill()
    daemon.wait(timeout=10)
    h.requests["late-expiry"]["expiryDate"] = date(3)
    h.references["late-expiry"] = "reserve16:" + secrets.token_hex(32)
    for name, connector in [("reuse-old", 1), ("reuse-new", 2)]:
        h.requests[name] = {"connectorId": connector, "expiryDate": date(300), "idTag": h.owner,
                            "parentIdTag": h.group, "reservationId": 119}
        h.references[name] = "reserve16:" + secrets.token_hex(32)
    h.requests["deadline-expiry"] = {"connectorId": 1, "expiryDate": date(20), "idTag": h.owner,
                                   "parentIdTag": h.group, "reservationId": 120}
    h.references["deadline-expiry"] = "reserve16:" + secrets.token_hex(32)
    h.write_provisioning()
    daemon = h.daemon()
    timestamp = date(1)
    h.relay.hold_next_start = True
    h.station([available(1), available(2), gate(), start("delayed-source-start", 1, h.owner, timestamp), stop("delayed-source-start-stop")])
    native_status(h, 1, "Available", wait_for)
    accepted(h.command("source-time-original", request="late-expiry"), require)
    require(h.relay.start_held.wait(timeout=10), "actual original start was not held")
    h.workflow(118, "expired")
    h.relay.release_start.set()
    wait_for(lambda: any(call["action"] == "StartTransaction" and call.get("reservationId") == 118 and "reply_elapsed_ms" in call for call in h.relay.calls), "held original start did not reach actual service")
    rows = [row for row in h.records() if row["reservation_id"] == 118]
    require(rows[-1]["state"] in ["consumed", "expired"], "delayed source start resurrected expired reservation")
    require(same_instant(rows[-1].get("source_time"), timestamp), "station source time conflated with trusted receipt")
    h.evidence["checks"].append("actual held pre-expiry start arrives after idle expiry; source time retained, no resurrection")

    # A delayed accepted ACK may arrive after independent native consumption.
    h.station([available(1), available(2), gate(), start("start-before-ack", 1, h.owner, current=True), stop("start-before-ack-stop")],
              pre=[step("late-ack", "delay_local_reply", duration_ms=800)])
    native_status(h, 1, "Available", wait_for)
    accepted(h.command("consume-before-late-ack", request="delayed"), require)
    h.workflow(116, "consumed")
    assert_current_source(h, 116, require)
    require(not h.native()["reservations"], "late ACK resurrected native reservation")
    h.evidence["checks"].append("real matched start before delayed mutation ACK remains consumed")
    mutation = h.relay.mutations()[-1]
    actual_starts = [call for call in h.relay.calls if call["action"] == "StartTransaction" and call.get("reservationId") == 116]
    require(actual_starts[-1]["observed_ns"] < mutation["reply_observed_ns"], "start did not precede actual delayed ACK")

    h.relay.hold_next_start = True
    h.station([available(1), available(2), gate(), start("old-reused-id-source", 1, h.owner, date(0.5)), stop("old-reused-id-source-stop")])
    accepted(h.command("reused-id-original", request="reuse-old"), require)
    require(h.relay.start_held.wait(timeout=10), "real reused-ID original start not held")
    time.sleep(0.8)
    outcome, errors = [], []
    def replace():
        try:
            outcome.append(h.command("reused-id-newer", request="reuse-new"))
        except Exception as error:
            errors.append(error)
    replacing = threading.Thread(target=replace)
    replacing.start()
    wait_for(lambda: h.native()["reservations"].get("119", {}).get("connectorId") == 2,
             "native newer reused-ID reservation did not install before original delivery")
    h.relay.release_start.set()
    replacing.join(timeout=15)
    require(not replacing.is_alive() and not errors and len(outcome) == 1, "real reused-ID replacement failed")
    accepted(outcome[0], require)
    h.workflow(119, "active")
    require([row for row in h.records() if row["reservation_id"] == 119][-1]["candidate"]["connector_id"] == 2,
            "delayed old start consumed a newer reused-ID revision")
    accepted(h.command("reused-id-cleanup", "CancelReservation", identifier=119), require)
    h.evidence["checks"].append("genuine held old source start cannot consume newer reused-ID revision")

    h.station([available(1), available(2)])
    h.relay.hold_next_mutation_ack = True
    h.command("ack-after-deadline-expiry", request="deadline-expiry", uncertain=True)
    require(h.relay.ack_held.is_set(), "real ACK was not held beyond the daemon deadline")
    h.workflow(120, "expired")
    h.relay.release_ack.set()
    def late_status():
        code, result = h.http("/api/v1/commands/ack-after-deadline-expiry", "read")
        return code == 200 and result.get("reservation_16", {}).get("status") == "Accepted"
    wait_for(late_status, "validated actual late ACK was not retained")
    h.workflow(120, "expired")
    require(not h.native()["reservations"], "late ACK recreated expired native reservation")
    h.evidence["checks"].append("real ACK beyond 30s deadline preserved as evidence without reviving expired revision")

    daemon.kill()
    daemon.wait(timeout=10)
    source = (h.output / "bridge.toml").read_text()
    zero_path = private(h.output / "bridge-zero-disabled.toml", source.replace("reserve_connector_zero_supported=true", "reserve_connector_zero_supported=false"))
    daemon = h.daemon(zero_path.name)
    h.station()
    h.command("bridge-zero-capability-off", request="any", deny=True)
    daemon.kill()
    daemon.wait(timeout=10)
    off_path = private(h.output / "bridge-actions-disabled.toml", source.replace("reserve_now=true", "reserve_now=false").replace("cancel_reservation=true", "cancel_reservation=false"))
    h.daemon(off_path.name)
    h.station()
    h.command("bridge-reserve-default-off", deny=True)
    h.command("bridge-cancel-default-off", "CancelReservation", identifier=120, deny=True)
