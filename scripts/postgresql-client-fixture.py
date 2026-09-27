#!/usr/bin/env python3
"""Single-use loopback PostgreSQL TLS gate for qualification only."""
import argparse
import socket
import ssl
import struct
import threading


def exact(stream, length):
    data = bytearray()
    while len(data) < length:
        fragment = stream.recv(length - len(data))
        if not fragment:
            raise EOFError
        data.extend(fragment)
    return bytes(data)


def frame(stream):
    header = exact(stream, 5)
    size = struct.unpack('!I', header[1:])[0]
    if not 4 <= size <= 1024 * 1024:
        raise ValueError('oversized PostgreSQL message')
    return header[0:1], header + exact(stream, size - 4)

def hostile(client, mode):
    try:
        if mode == 'hostile-header':
            # Advertise 2 GiB without allocating or sending its body.
            client.sendall(b'N' + struct.pack('!I', 0x7fffffff))
        else:
            # The setup batch accepts CommandComplete messages; none ends the response.
            command = b'SET ' + b'x' * 32758 + b'\0'
            packet = b'C' + struct.pack('!I', len(command) + 4) + command
            for _ in range(65):
                client.sendall(packet)
    except OSError:
        # A rejection can race with the last write.
        pass
    print(f'hostile_sent={mode}', flush=True)
    # Require client-initiated teardown, not just a query deadline.
    try:
        if client.recv(1) != b'':
            raise RuntimeError('client did not close hostile connection')
    except (ssl.SSLEOFError, ConnectionResetError):
        pass
    print('client_closed=1', flush=True)


def gate(client, upstream, mode):
    pending_commit = threading.Event()
    pending_insert = threading.Event()
    finished = threading.Event()

    def request():
        try:
            while not finished.is_set():
                kind, message = frame(client)
                if mode.startswith('hostile-') and kind == b'Q':
                    hostile(client, mode)
                    finished.set()
                    upstream.shutdown(socket.SHUT_RDWR)
                    break
                if kind == b'P' and b'INSERT INTO public.qualification_markers' in message:
                    pending_insert.set()
                if mode == 'insert-loss' and kind == b'E' and pending_insert.is_set():
                    upstream.sendall(message)
                    print('insert_submitted=1', flush=True)
                    finished.set()
                    upstream.shutdown(socket.SHUT_RDWR)
                    break
                if kind == b'Q' and message[5:] == b'COMMIT\0':
                    if mode == 'before-commit-loss':
                        print('commit_not_forwarded=1', flush=True)
                        finished.set()
                        upstream.shutdown(socket.SHUT_RDWR)
                        break
                    pending_commit.set()
                upstream.sendall(message)
        except (EOFError, OSError, ValueError):
            finished.set()
            try:
                upstream.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass

    worker = threading.Thread(target=request, daemon=True)
    worker.start()
    try:
        while not finished.is_set():
            kind, message = frame(upstream)
            if mode == 'commit-loss' and pending_commit.is_set():
                if kind == b'Z':
                    print('commit_ack_suppressed=1', flush=True)
                    break
            else:
                client.sendall(message)
    except (EOFError, OSError, ValueError):
        pass
    finally:
        finished.set()
        for stream in (client, upstream):
            try:
                stream.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            stream.close()
        worker.join(timeout=1)
        if worker.is_alive():
            raise RuntimeError('proxy worker failed to stop')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('mode', choices=('hostile-header', 'hostile-aggregate', 'commit-loss', 'before-commit-loss', 'insert-loss', 'recovery', 'refuse', 'stall', 'tls-stall', 'auth-stall', 'startup-reset', 'startup-close'))
    parser.add_argument('upstream', help='numeric IP:port of disposable PostgreSQL')
    parser.add_argument('cert')
    parser.add_argument('key')
    parser.add_argument('ca')
    parser.add_argument('--cycles', type=int, default=1)
    args = parser.parse_args()
    host, port = args.upstream.rsplit(':', 1)
    server_tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    server_tls.load_cert_chain(args.cert, args.key)
    client_tls = ssl.create_default_context(cafile=args.ca)
    if args.cycles < 1 or args.cycles > 1000:
        parser.error('cycles must be in 1..1000')
    listener = socket.create_server(('127.0.0.1', 0), backlog=2)
    listener.settimeout(20)
    print(f'port={listener.getsockname()[1]}', flush=True)
    try:
        for _ in range(args.cycles):
            attempts = 2 if args.mode == 'recovery' else 1
            for attempt in range(attempts):
                incoming, _ = listener.accept()
                incoming.settimeout(10)
                with incoming:
                    if args.mode == 'stall':
                        threading.Event().wait(7)
                        return
                    if exact(incoming, 8) != b'\x00\x00\x00\x08\x04\xd2\x16\x2f':
                        raise ValueError('unexpected PostgreSQL startup')
                    if args.mode == 'refuse' or (args.mode == 'recovery' and attempt == 0):
                        incoming.sendall(b'N')
                        continue
                    incoming.sendall(b'S')
                    if args.mode == 'tls-stall':
                        threading.Event().wait(7)
                        return
                    with server_tls.wrap_socket(incoming, server_side=True) as client:
                        if args.mode == 'auth-stall':
                            threading.Event().wait(7)
                            return
                        length = struct.unpack('!I', exact(client, 4))[0]
                        if not 8 <= length <= 65536:
                            raise ValueError('oversized startup packet')
                        startup = struct.pack('!I', length) + exact(client, length - 4)
                        if args.mode == 'startup-reset':
                            client.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack('ii', 1, 0))
                            print('startup_reset=1', flush=True)
                            continue
                        if args.mode == 'startup-close':
                            # Send TLS close_notify for an orderly EOF; the client need not reciprocate.
                            try:
                                client.unwrap().close()
                            except ssl.SSLEOFError:
                                pass
                            print('startup_closed=1', flush=True)
                            continue
                        with socket.create_connection((host, int(port)), timeout=5) as remote:
                            remote.sendall(b'\x00\x00\x00\x08\x04\xd2\x16\x2f')
                            if exact(remote, 1) != b'S':
                                raise ValueError('upstream TLS refused')
                            with client_tls.wrap_socket(remote, server_hostname='localhost') as upstream:
                                upstream.sendall(startup)
                                gate(client, upstream, args.mode)
        print(f'completed_cycles={args.cycles}', flush=True)
    finally:
        listener.close()


if __name__ == '__main__':
    main()
