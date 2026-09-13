"""Isolated Mobility House CSMS policy; protocol parsing/validation stays upstream."""

import asyncio

from ocpp.routing import on
from ocpp.v16 import ChargePoint as ChargePoint16, call as call16, call_result as result16
from ocpp.v201 import ChargePoint as ChargePoint201, call as call201
from ocpp.v201 import call_result as result201

NOW = "2026-09-01T00:00:00Z"


class Policy:
    def initialize(self, mismatch):
        self.mismatch = mismatch
        self.observed = []
        self.authorized = asyncio.Event()
        self.metered = asyncio.Event()
        self.remote_start_accepted = asyncio.Event()
        self.remote_stop_accepted = asyncio.Event()

    def record(self, action):
        self.observed.append(action)

    @on("BootNotification")
    def boot(self, **kwargs):
        self.record("BootNotification")
        return self.results.BootNotification(
            current_time=NOW, interval=60,
            status="Rejected" if self.mismatch else "Accepted",
        )

    @on("Heartbeat")
    def heartbeat(self):
        self.record("Heartbeat")
        return self.results.Heartbeat(current_time=NOW)

    @on("Authorize")
    def authorize(self, **kwargs):
        self.record("Authorize")
        self.authorized.set()
        return self.results.Authorize(**{self.token_info: {"status": "Accepted"}})

    @on("StatusNotification")
    def status(self, **kwargs):
        self.record("StatusNotification")
        return self.results.StatusNotification()

    async def remote_commands(self):
        await self.authorized.wait()
        response = await self.call(self.start_request())
        if response.status != "Accepted":
            raise ValueError("remote start rejected")
        self.record(self.start_name + ":Accepted")
        self.remote_start_accepted.set()
        await self.metered.wait()
        response = await self.call(self.stop_request())
        if response.status != "Accepted":
            raise ValueError("remote stop rejected")
        self.record(self.stop_name + ":Accepted")
        self.remote_stop_accepted.set()


class Peer16(Policy, ChargePoint16):
    results = result16
    token_info = "id_tag_info"
    start_name = "RemoteStartTransaction"
    stop_name = "RemoteStopTransaction"

    def start_request(self):
        return call16.RemoteStartTransaction(id_tag="LOCAL-USER-1", connector_id=1)

    def stop_request(self):
        return call16.RemoteStopTransaction(transaction_id=42)

    @on("StartTransaction")
    async def start_transaction(self, **kwargs):
        await asyncio.wait_for(self.remote_start_accepted.wait(), 5)
        self.record("StartTransaction")
        return result16.StartTransaction(
            transaction_id=42, id_tag_info={"status": "Accepted"},
        )

    @on("MeterValues")
    def meter(self, transaction_id, **kwargs):
        if transaction_id != 42:
            raise ValueError("unexpected transaction")
        self.record("MeterValues")
        self.metered.set()
        return result16.MeterValues()

    @on("StopTransaction")
    async def stop_transaction(self, transaction_id, **kwargs):
        await asyncio.wait_for(self.remote_stop_accepted.wait(), 5)
        if transaction_id != 42:
            raise ValueError("unexpected transaction")
        self.record("StopTransaction")
        return result16.StopTransaction()


class Peer201(Policy, ChargePoint201):
    results = result201
    token_info = "id_token_info"
    start_name = "RequestStartTransaction"
    stop_name = "RequestStopTransaction"

    def start_request(self):
        return call201.RequestStartTransaction(
            id_token={"idToken": "LOCAL-USER-201", "type": "Central"},
            remote_start_id=171, evse_id=2,
        )

    def stop_request(self):
        return call201.RequestStopTransaction(transaction_id="tx-201-42")

    @on("TransactionEvent")
    async def transaction(self, event_type, seq_no, transaction_info, evse, **kwargs):
        expected = {"Started": 0, "Updated": 1, "Ended": 2}
        if (seq_no != expected[event_type]
                or transaction_info["transaction_id"] != "tx-201-42"
                or evse != {"id": 2, "connector_id": 1}):
            raise ValueError("unexpected transaction identity or sequence")
        if event_type == "Started":
            await asyncio.wait_for(self.remote_start_accepted.wait(), 5)
        if event_type == "Ended":
            await asyncio.wait_for(self.remote_stop_accepted.wait(), 5)
        self.record("TransactionEvent:" + event_type)
        if event_type == "Started":
            return result201.TransactionEvent(id_token_info={"status": "Accepted"})
        if event_type == "Updated":
            self.metered.set()
        return result201.TransactionEvent()
