"""Transparent loopback TCP relay for 2.0.1 frames; observes, never replies.

Only direction, action, connection number, hashed message ID and a fixed set of
non-identity fields are retained. No idToken, groupIdToken, credential or raw frame
is logged or persisted.
"""
import hashlib
import socket
import threading
import time

from reservation16_joint_transport import FrameObserver

RECORDED = {
    "ReserveNow": ["id", "evseId"],
    "CancelReservation": ["reservationId"],
    "StatusNotification": ["connectorStatus", "evseId", "connectorId"],
    "TransactionEvent": ["eventType", "reservationId", "timestamp"],
    "ReservationStatusUpdate": ["reservationId", "reservationUpdateStatus"],
}
TRACKED = ["BootNotification", "Authorize", *RECORDED]


class NativeWireRelay201:
    def __init__(self, upstream):
        self.upstream = upstream
        self.listener = socket.socket()
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen()
        self.port = self.listener.getsockname()[1]
        self.lock = threading.Lock()
        self.calls, self.errors, self.boots = [], [], []
        self.sockets, self.pending = set(), {}
        self.generation = 0
        self.stopping = False
        self.thread = threading.Thread(target=self.accept, daemon=True)
        self.thread.start()

    def accept(self):
        while not self.stopping:
            try:
                station, _ = self.listener.accept()
                bridge = socket.create_connection(("127.0.0.1", self.upstream), timeout=3)
                bridge.settimeout(None)
            except OSError:
                if not self.stopping:
                    with self.lock:
                        self.errors.append("native relay connection unavailable")
                continue
            with self.lock:
                self.sockets.update([station, bridge])
                self.generation += 1
                generation = self.generation
            for source, target, direction in [(station, bridge, "station"), (bridge, station, "csms")]:
                threading.Thread(target=self.pipe, args=(source, target, direction, generation),
                                 daemon=True).start()

    def record(self, frame, direction, generation):
        if not isinstance(frame, list) or len(frame) < 2 or not isinstance(frame[1], str):
            return
        identity = hashlib.sha256(frame[1].encode()).hexdigest()
        with self.lock:
            if frame[0] == 2 and len(frame) == 4 and isinstance(frame[2], str):
                if len(self.calls) >= 4096:
                    raise ValueError("native observation count bound")
                call = {"connection": generation, "direction": direction, "action": frame[2],
                        "message_id_sha256": identity, "observed_ns": time.monotonic_ns()}
                for key in RECORDED.get(frame[2], []):
                    if isinstance(frame[3], dict) and key in frame[3]:
                        call[key] = frame[3][key]
                if isinstance(frame[3], dict) and frame[2] == "TransactionEvent":
                    call["idTokenPresent"] = "idToken" in frame[3]
                self.calls.append(call)
                if frame[2] in TRACKED:
                    if len(self.pending) >= 128:
                        raise ValueError("native observation pending bound")
                    self.pending[(generation, direction, identity)] = (call, time.monotonic())
            elif frame[0] in [3, 4]:
                opposite = "station" if direction == "csms" else "csms"
                pending = self.pending.pop((generation, opposite, identity), None)
                if pending:
                    call, sent = pending
                    call["reply_elapsed_ms"] = int((time.monotonic() - sent) * 1000)
                    call["reply_type"] = frame[0]
                    status = frame[2].get("status") if frame[0] == 3 and isinstance(frame[2], dict) else None
                    if status in ["Accepted", "Rejected", "Faulted", "Occupied", "Unavailable"]:
                        call["reply_status"] = status
                    if call["action"] == "BootNotification" and status == "Accepted":
                        self.boots.append(generation)

    def pipe(self, source, target, direction, generation):
        observer = FrameObserver(lambda frame: self.record(frame, direction, generation))
        try:
            while data := source.recv(16384):
                observer.feed(data)
                target.sendall(data)
        except OSError:
            pass  # Actual process death/connection loss is part of this smoke.
        except (ValueError, UnicodeError):
            with self.lock:
                self.errors.append("native frame observation failed")
        finally:
            for connection in [source, target]:
                try:
                    connection.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass
                connection.close()
                with self.lock:
                    self.sockets.discard(connection)

    def observed(self, direction=None, action=None):
        with self.lock:
            if self.errors:
                raise RuntimeError(self.errors[0])
            return [dict(call) for call in self.calls
                    if (direction is None or call["direction"] == direction)
                    and (action is None or call["action"] == action)]

    def mutations(self):
        return [call for call in self.observed("csms")
                if call["action"] in ["ReserveNow", "CancelReservation"]]

    def accepted_boots(self):
        with self.lock:
            return list(self.boots)

    def close(self):
        self.stopping = True
        self.listener.close()
        with self.lock:
            connections = list(self.sockets)
        for connection in connections:
            try:
                connection.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            connection.close()
