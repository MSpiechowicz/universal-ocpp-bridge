"""Independent native 2.0.1 scenarios and real durable/public reservation acceptance phases."""
from datetime import datetime


def step(name, action, **fields):
    return {"id": name, "station": "alpha", "action": action, "timeout_ms": 10000, **fields}


def boot():
    return step("boot", "boot", fixture_id="sim.ocpp201.boot.accepted",
                payload={"reason": "PowerUp", "chargingStation": {"model": "Reservations", "vendorName": "UOB"}})


def parked():
    return step("park", "wait", duration_ms=60000, timeout_ms=65000)


def status(evse, value):
    return step(f"{value.lower()}-{evse}", "status", fixture_id="sim.ocpp201.status." + value.lower(),
                payload={"timestamp": "2026-10-06T00:00:00Z", "connectorStatus": value, "evseId": evse,
                         "connectorId": 1})


def authorize(name, token):
    return step(name, "authorize", fixture_id="sim.ocpp201.authorize.accepted",
                payload={"idToken": {"idToken": token, "type": "ISO14443"}},
                expect_response={"idTokenInfo": {"status": "Accepted"}})


def gate(count, name="gate"):
    return step(name, "await_reservation", timeout_ms=30000, expect_response={"activeReservations": count})


def start(name, token, transaction):
    return step(name, "start_transaction", fixture_id="sim.ocpp201.transaction.started", use_current_timestamp=True,
                payload={"eventType": "Started", "triggerReason": "Authorized", "seqNo": 0,
                         "transactionInfo": {"transactionId": transaction, "chargingState": "EVConnected"},
                         "evse": {"id": 1, "connectorId": 1}, "idToken": {"idToken": token, "type": "ISO14443"}})


def updates(h, status_value, identifier):
    return [call for call in h.relay.observed("station", "ReservationStatusUpdate")
            if call.get("reservationUpdateStatus") == status_value and call.get("reservationId") == identifier
            and call.get("reply_type") == 3]


def reported(h, generation, evse, value):
    return any(call["connection"] >= generation and call.get("evseId") == evse
               and call.get("connectorStatus") == value
               for call in h.relay.observed("station", "StatusNotification"))


def denials(h):
    h.command("nonprivileged-reserve", request="exact", grant="read")
    h.command("control-grant-reserve", request="exact", grant="control")
    h.command("raw-public-token", request="exact", override={"idToken": {"idToken": h.owner, "type": "ISO14443"}}, deny=True)
    h.command("raw-public-group", request="exact", override={"groupIdToken": {"idToken": h.group, "type": "Central"}}, deny=True)
    h.command("immutable-id-mismatch", request="exact", override={"id": 999}, deny=True)
    h.command("signed-id-overflow", request="exact", override={"id": 2147483648}, deny=True)
    h.command("evse-scope-mismatch", request="exact", override={"evseId": 2}, deny=True)
    h.command("unowned-reference", request="exact", override={"reservationReference": "reserve201:" + "f" * 64}, deny=True)
    h.command("legacy-edition-schema", request="exact", schema="urn:uob:ocpp16:ReserveNowReference:1", deny=True)
    h.command("wrong-station", request="exact", scope={"station_id": "not-alpha"}, deny=True)
    h.command("evse-scoped-cancel", action="CancelReservation", identifier=-114,
              scope={"resource": {"kind": "evse", "evse_id": "one"},
                     "native_protocol_reference": {"protocol": "ocpp201", "evse_id": 1}}, deny=True)
    h.command("cancel-vendor-data", action="CancelReservation", identifier=-114,
              override={"customData": {"vendorId": "vendor"}}, deny=True)


def creation(h, require, wait_for, generation):
    h.command("exact", status="Accepted")
    wait_for(lambda: reported(h, generation, 1, "Reserved"), "H01.FR.23 Reserved report missing")
    h.check("station reports Reserved for the exact EVSE")
    h.workflow(-114, "active")
    h.command("occupied", status="Occupied")
    h.workflow(0, "rejected")
    h.command("replacement", status="Accepted")
    rows = h.workflow(-114, "active")
    require([row["state"] for row in rows] == ["superseded", "active"], "same-id replacement owner")
    require(rows[-1]["candidate"]["evse_id"] == 2, "replacement EVSE")
    for name in ["min", "max"]:
        h.command("cancel-before-" + name, action="CancelReservation", identifier=h.requests[name]["id"],
                  status="Rejected")
    h.command("unspecified", status="Accepted")
    h.workflow(7, "active")
    require(h.records()[-1]["candidate"]["evse_id"] is None, "unspecified reservation never binds an EVSE")
    h.command("cancel-unknown", action="CancelReservation", identifier=999, status="Rejected")
    h.command("cancel-unspecified", action="CancelReservation", identifier=7, status="Accepted")
    h.workflow(7, "cancelled")
    h.command("max", status="Accepted")
    h.workflow(2147483647, "active")
    h.command("cancel-max", action="CancelReservation", identifier=2147483647, status="Accepted")
    h.workflow(2147483647, "cancelled")
    require(not [call for call in h.relay.observed("station", "ReservationStatusUpdate")
                 if call.get("reservationId") == 7],
            "H02 remark: CSMS cancellation produced a station update")
    h.check("no ReservationStatusUpdate for CSMS cancellation")


def removal(h, require, wait_for):
    h.station([status(2, "Faulted"), gate(0, "removed")])
    wait_for(lambda: updates(h, "Removed", -114), "H01.FR.16 Removed update missing")
    rows = h.workflow(-114, "removed")
    require(rows[-1]["source_time"] is None, "native update carries no station time")
    # H01.FR.12: the actual faulted EVSE answers Faulted; the signed minimum id survives.
    h.command("min", status="Faulted")
    h.workflow(-2147483648, "rejected")


def use(h, require, wait_for):
    h.station([authorize("authorize-owner", h.owner), gate(1, "use-gate"), start("reserved-start", h.owner, "tx-114"),
               step("terminated", "assert_reservation", expect_response={"activeReservations": 0}),
               # The bridge never grants authorization from a transaction report, so the station
               # deauthorizes; the cable is then unplugged and the EVSE reports Available.
               status(1, "Available")])
    h.command("use", status="Accepted")
    starts = []
    def started():
        starts[:] = [call for call in h.relay.observed("station", "TransactionEvent")
                     if call.get("reservationId") == 114 and call.get("idTokenPresent")]
        return bool(starts)
    wait_for(started, "H01.FR.15 reservationId TransactionEvent missing", 15)
    rows = h.workflow(114, "consumed")
    source = datetime.fromisoformat(starts[-1]["timestamp"].replace("Z", "+00:00"))
    durable = datetime.fromisoformat(rows[-1]["source_time"].replace("Z", "+00:00"))
    require(source == durable, "durable source evidence differs from the actual native start")
    h.check("consumed with the actual native TransactionEvent timestamp")


def expiry(h, require, wait_for):
    # Provisioned expiries are immutable owner data, so this short one runs first.
    h.command("expiry", status="Accepted")
    h.workflow(115, "active")
    wait_for(lambda: updates(h, "Expired", 115), "H04.FR.01 Expired update missing", 15)
    h.workflow(115, "expired")


def dropped(h, daemon, require, wait_for):
    h.station([step("drop-ack", "drop_local_reply")])
    h.command("drop", uncertain=True)
    h.workflow(117, "uncertain")
    sent = len([call for call in h.relay.mutations() if call["action"] == "ReserveNow" and call.get("id") == 117])
    require(sent == 1, "dropped ACK was not one real ReserveNow")
    daemon.kill()
    daemon.wait(timeout=10)
    restarted = h.daemon()
    h.station()
    h.workflow(117, "uncertain")
    require(len([call for call in h.relay.mutations() if call["action"] == "ReserveNow" and call.get("id") == 117]) == 1,
            "restart replayed an uncertain native mutation")
    h.check("restart without mutator replay")
    h.command("cancel-dropped", action="CancelReservation", identifier=117, status="Accepted")
    h.workflow(117, "cancelled")
    return restarted


def run_flow(h, daemon, require, wait_for):
    generation = h.station()
    expiry(h, require, wait_for)
    denials(h)
    creation(h, require, wait_for, generation)
    removal(h, require, wait_for)
    use(h, require, wait_for)
    dropped(h, daemon, require, wait_for)
