"""Transparent loopback TCP relay; observes real OCPP frames, never replies.

Only direction, action, connection number and hashed message ID are retained.
No token, payload, HTTP credential or raw frame is logged or persisted.
"""
import hashlib
import json
import socket
import threading
import time


class FrameObserver:
    def __init__(self, record):
        self.record = record
        self.buffer = bytearray()
        self.fragment = bytearray()
        self.upgraded = False

    def feed(self, data):
        self.buffer.extend(data)
        if not self.upgraded:
            boundary = self.buffer.find(b"\r\n\r\n")
            if boundary < 0:
                if len(self.buffer) > 16384:
                    raise ValueError("native HTTP observation bound")
                return
            del self.buffer[:boundary + 4]
            self.upgraded = True
        while len(self.buffer) >= 2:
            first, second = self.buffer[:2]
            size = second & 127
            offset = 2
            if size in [126, 127]:
                width = 2 if size == 126 else 8
                if len(self.buffer) < offset + width:
                    return
                size = int.from_bytes(self.buffer[offset:offset + width], "big")
                offset += width
            if size > 128 * 1024:
                raise ValueError("native frame observation bound")
            mask = None
            if second & 128:
                if len(self.buffer) < offset + 4:
                    return
                mask = bytes(self.buffer[offset:offset + 4])
                offset += 4
            if len(self.buffer) < offset + size:
                return
            payload = bytearray(self.buffer[offset:offset + size])
            del self.buffer[:offset + size]
            if mask:
                for index in range(len(payload)):
                    payload[index] ^= mask[index % 4]
            opcode = first & 15
            if opcode in [0, 1]:
                self.fragment.extend(payload)
                if len(self.fragment) > 128 * 1024:
                    raise ValueError("native fragmented observation bound")
                if first & 128:
                    value = json.loads(self.fragment)
                    self.fragment[:] = b"\0" * len(self.fragment)
                    self.fragment.clear()
                    self.record(value)
            payload[:] = b"\0" * len(payload)


class NativeWireRelay:
    def __init__(self, upstream):
        self.upstream = upstream
        self.listener = socket.socket()
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen()
        self.port = self.listener.getsockname()[1]
        self.lock = threading.Lock()
        self.calls = []
        self.errors = []
        self.sockets = set()
        self.generation = 0
        self.stopping = False
        self.pending = {}
        self.boots = []
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
                threading.Thread(target=self.pipe, args=(source, target, direction, generation), daemon=True).start()

    def pipe(self, source, target, direction, generation):
        def record(frame):
            if not isinstance(frame, list) or len(frame) < 2 or not isinstance(frame[1], str):
                return
            identity = hashlib.sha256(frame[1].encode()).hexdigest()
            with self.lock:
                if frame[0] == 2 and len(frame) == 4 and isinstance(frame[2], str):
                    if len(self.calls) >= 4096:
                        raise ValueError("native observation count bound")
                    call = {"connection": generation, "direction": direction,
                            "action": frame[2], "message_id_sha256": identity}
                    self.calls.append(call)
                    if frame[2] in ["BootNotification", "SendLocalList", "ClearCache"]:
                        if len(self.pending) >= 128:
                            raise ValueError("native observation pending bound")
                        self.pending[(generation, direction, identity)] = (call, time.monotonic())
                elif frame[0] in [3, 4]:
                    opposite = "station" if direction == "csms" else "csms"
                    pending = self.pending.pop((generation, opposite, identity), None)
                    if pending:
                        call, sent = pending
                        call["reply_elapsed_ms"] = int((time.monotonic() - sent) * 1000)
                        status = frame[2].get("status") if frame[0] == 3 and isinstance(frame[2], dict) else None
                        if status in ["Accepted", "Rejected", "Pending", "Failed", "VersionMismatch"]:
                            call["reply_status"] = status
                        if call["action"] == "BootNotification" and status == "Accepted":
                            self.boots.append(generation)
        observer = FrameObserver(record)
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

    def mutations(self):
        with self.lock:
            if self.errors:
                raise RuntimeError(self.errors[0])
            return [dict(call) for call in self.calls if call["direction"] == "csms"
                    and call["action"] in ["SendLocalList", "ClearCache"]]

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
