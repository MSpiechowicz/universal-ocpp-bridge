#!/usr/bin/env python3
"""Execute extracted runtime packages, recording native runner and archive identity."""
import argparse
import concurrent.futures
import contextlib
import json
import os
import pathlib
import platform
import re
import resource
import signal
import subprocess
import tarfile
import tempfile
import time
import urllib.error
import urllib.request

from package_smoke_peer import heartbeat_peer, listener
from platform_packages import TARGETS, audit, digest

MAX_OUTPUT = 1024 * 1024
HTTP = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def run(command, cwd):
    # Regular bounded files avoid pipe deadlocks; per-process output is limited on Linux.
    with tempfile.TemporaryFile() as output, tempfile.TemporaryFile() as errors:
        process = subprocess.run(command, cwd=cwd, stdin=subprocess.DEVNULL,
                                 stdout=output, stderr=errors, timeout=30,
                                 env={"PATH": "/nonexistent"})
        output.seek(0)
        body = output.read(MAX_OUTPUT + 1)
        if process.returncode or len(body) > MAX_OUTPUT:
            raise ValueError("packaged command failed or exceeded output limit")
        return body


@contextlib.contextmanager
def limit_output():
    previous = resource.getrlimit(resource.RLIMIT_FSIZE)
    resource.setrlimit(resource.RLIMIT_FSIZE, (MAX_OUTPUT, previous[1]))
    try:
        yield
    finally:
        resource.setrlimit(resource.RLIMIT_FSIZE, previous)


def get(port, path, expected_status=200):
    try:
        response = HTTP.open(f"http://127.0.0.1:{port}{path}", timeout=1)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        body = response.read(MAX_OUTPUT + 1)
        if response.status != expected_status or len(body) > MAX_OUTPUT:
            raise ValueError("invalid or oversized HTTP response")
        return body


def service_smoke(binary, directory):
    with listener() as reserved:
        port = reserved.getsockname()[1]
    config = directory / "bridge.toml"
    config.write_text('[bridge]\nid="package-smoke"\nenvironment="demo"\n'
                      f'[management]\nlisten_addr="127.0.0.1:{port}"\n')
    if json.loads(run([str(binary), "config", "check", "--config", str(config)], directory)) != {"status": "valid"}:
        raise ValueError("offline configuration validation failed")
    with tempfile.TemporaryFile() as log:
        child = subprocess.Popen([str(binary), "serve", "--config", str(config)],
                                 cwd=directory, stdin=subprocess.DEVNULL, stdout=log, stderr=log,
                                 env={"PATH": "/nonexistent"})
        try:
            deadline = time.monotonic() + 20
            while True:
                if child.poll() is not None:
                    raise ValueError("daemon exited during startup")
                try:
                    health = json.loads(get(port, "/health", expected_status=503))
                    break
                except (OSError, urllib.error.URLError):
                    if time.monotonic() >= deadline:
                        raise ValueError("daemon health deadline exceeded") from None
                    time.sleep(0.1)
            if {key: health.get(key) for key in ("readiness", "core_loop", "storage")} != {
                    "readiness": "not_ready", "core_loop": "starting", "storage": "starting"}:
                raise ValueError("unexpected management-only health state")
            identity = json.loads(get(port, "/api/v1/identity"))
            if not isinstance(identity, dict):
                raise ValueError("invalid identity document")
            html = get(port, "/").decode()
            assets = re.findall(r'(?:src|href)="(/ui/assets/[^"?#]+\.(?:js|css))"', html)
            if not assets or not any(asset.endswith(".js") for asset in assets):
                raise ValueError("missing embedded browser assets")
            for asset in assets:
                if not get(port, asset):
                    raise ValueError("empty browser asset")
            child.send_signal(signal.SIGTERM)
            if child.wait(timeout=25) != 0:
                raise ValueError("daemon shutdown failed")
        finally:
            if child.poll() is None:
                child.kill()
                child.wait(timeout=5)
    return {"status": "passed", "health": {"http_status": 503, "readiness": "not_ready"},
            "checks": ["config", "startup", "health", "identity",
                                             "embedded_assets", "graceful_shutdown"]}


def simulator_smoke(binary, directory):
    results = []
    for version, protocol in [("1.6", "ocpp1.6"), ("2.0.1", "ocpp2.0.1")]:
        with listener() as peer, concurrent.futures.ThreadPoolExecutor(max_workers=1) as executor:
            config = directory / "simulator.toml"
            config.write_text('schema_version=1\n[[stations]]\nid="alpha"\n'
                              f'endpoint="ws://127.0.0.1:{peer.getsockname()[1]}/alpha"\n'
                              f'ocpp_version="{version}"\nrequest_timeout_ms=5000\n')
            scenario = directory / "scenario.toml"
            scenario.write_text('schema_version=1\nseed=42\n' + ''.join(
                f'[[steps]]\nid="{action}"\nstation="alpha"\naction="{action}"\ntimeout_ms=5000\n'
                + ('expect_event="heartbeat_result"\nexpect_detail="2026-09-01T00:00:00Z"\n'
                   if action == "heartbeat" else '')
                for action in ["connect", "heartbeat", "disconnect"]))
            future = executor.submit(heartbeat_peer, peer, protocol)
            output = run([str(binary), "run", "--config", str(config), "--scenario",
                          str(scenario), "--seed", "42", "--format", "jsonl"], directory)
            records = [json.loads(line) for line in output.splitlines()]
            if not records or any(record.get("event") == "run_failed" for record in records):
                raise ValueError("simulator did not complete successfully")
            results.append(future.result(timeout=15))
    return {"status": "passed", "exchanges": results}


def smoke(packages, report):
    target = platform.machine() + "-unknown-linux-gnu"
    if platform.system() != "Linux" or target not in TARGETS:
        raise ValueError("unsupported execution host")
    with tempfile.TemporaryDirectory(prefix="uob-package-smoke-") as temporary:
        for kind, binary in [("service", "uob"), ("simulator", "uob-sim")]:
            candidates = list(packages.glob(f"{binary}-[0-9]*-{target}.tar.gz"))
            if len(candidates) != 1:
                raise ValueError("expected exactly one archive per kind and native target")
            archive = candidates[0]
            manifest = audit(archive, kind, target)
            checksum = digest(archive.read_bytes())
            if archive.with_suffix(".gz.sha256").read_text() != f"{checksum}  {archive.name}\n":
                raise ValueError("archive checksum mismatch")
            report["packages"][kind] = {"archive": archive.name, "sha256": checksum,
                                        "manifest": manifest, "status": "failed"}
            directory = pathlib.Path(temporary) / kind
            directory.mkdir()
            with tarfile.open(archive) as package:
                package.extractall(directory, filter="data")
            check = service_smoke if kind == "service" else simulator_smoke
            with limit_output():
                result = check(directory / "bin" / binary, directory)
            report["packages"][kind].update(result)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--packages", type=pathlib.Path, required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    args = parser.parse_args()
    report = {"schema_version": 1, "status": "failed", "packages": {},
              "pi_performance": "unqualified", "execution_mode": "native",
              "runner": {"machine": platform.machine(), "kernel": platform.release(),
                         "os": platform.freedesktop_os_release(),
                         **{key: os.environ.get(key) for key in ["RUNNER_NAME", "RUNNER_ARCH",
                            "RUNNER_OS", "ImageOS", "ImageVersion", "GITHUB_RUN_ID",
                            "GITHUB_RUN_ATTEMPT", "GITHUB_SHA"]}}}
    try:
        smoke(args.packages.resolve(), report)
        report["status"] = "passed"
    except Exception as error:
        report["failure"] = type(error).__name__
        raise
    finally:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
