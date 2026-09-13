"""Bounded independent OCPP Heartbeat peer, used only by package smoke checks."""
import base64
import hashlib
import json
import socket


def receive(stream, count):
    data = b""
    while len(data) < count:
        chunk = stream.recv(count - len(data))
        if not chunk:
            raise ValueError("unexpected peer EOF")
        data += chunk
    return data


def heartbeat_peer(listener, protocol):
    listener.settimeout(15)
    with listener.accept()[0] as stream:
        stream.settimeout(10)
        header = b""
        while not header.endswith(b"\r\n\r\n"):
            header += receive(stream, 1)
            if len(header) > 8192:
                raise ValueError("oversized handshake")
        lines = header.decode("ascii").split("\r\n")
        fields = dict(line.lower().split(": ", 1) for line in lines[1:] if ": " in line)
        # Header values (notably the random WebSocket key) are case sensitive.
        key = next(line.split(":", 1)[1].strip() for line in lines
                   if line.lower().startswith("sec-websocket-key:"))
        if fields.get("sec-websocket-protocol") != protocol:
            raise ValueError("incorrect negotiated OCPP protocol")
        accept = base64.b64encode(hashlib.sha1(
            (key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()).decode()
        stream.sendall(("HTTP/1.1 101 Switching Protocols\r\n"
                        "Upgrade: websocket\r\nConnection: Upgrade\r\n"
                        f"Sec-WebSocket-Accept: {accept}\r\n"
                        f"Sec-WebSocket-Protocol: {protocol}\r\n\r\n").encode())
        first, second = receive(stream, 2)
        if first != 0x81 or second & 0x80 == 0:
            raise ValueError("expected one masked text frame")
        length = second & 0x7f
        if length == 126:
            length = int.from_bytes(receive(stream, 2), "big")
        if length == 127 or length > 4096:
            raise ValueError("oversized heartbeat")
        mask = receive(stream, 4)
        payload = receive(stream, length)
        call = json.loads(bytes(value ^ mask[i % 4] for i, value in enumerate(payload)))
        if len(call) != 4 or call[0] != 2 or call[2:] != ["Heartbeat", {}]:
            raise ValueError("expected native Heartbeat CALL")
        response = json.dumps([3, call[1], {"currentTime": "2026-09-01T00:00:00Z"}]).encode()
        if len(response) > 125:
            raise ValueError("unexpected correlation identifier size")
        stream.sendall(bytes([0x81, len(response)]) + response)
        # The scenario ends with a clean client close after its response assertion.
        first, second = receive(stream, 2)
        if first != 0x88 or second & 0x7f > 125:
            raise ValueError("expected bounded close frame")
        length = second & 0x7f
        mask = receive(stream, 4) if second & 0x80 else b"\0" * 4
        payload = receive(stream, length)
        payload = bytes(value ^ mask[i % 4] for i, value in enumerate(payload))
        stream.sendall(bytes([0x88, len(payload)]) + payload)
    return {"protocol": protocol, "action": "Heartbeat", "status": "passed"}


def listener():
    result = socket.socket()
    result.bind(("127.0.0.1", 0))
    result.listen(1)
    return result
