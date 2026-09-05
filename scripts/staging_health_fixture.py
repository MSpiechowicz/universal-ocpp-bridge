#!/usr/bin/env python3
"""Synthetic production sentinel for the disposable cgroup harness only."""
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
from pathlib import Path
import sys

root = Path(sys.argv[1])


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        payload = json.dumps({
            'readiness': 'ready', 'core_loop': 'ready', 'storage': 'safe',
            'accepts_new_sessions': True,
            'local_response_latency': {
                'p95_upper_bound_ms': 101 if (root / 'alarm').exists() else 0},
            'daemon_process': {'rss_bytes': 1024},
        }).encode()
        self.send_response(200)
        self.send_header('Content-Length', str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *_args):
        pass


server = HTTPServer(('127.0.0.1', 0), Handler)
(root / 'port').write_text(str(server.server_port))
server.serve_forever()
