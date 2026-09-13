#!/usr/bin/env python3
"""Optional external-stack smoke runner. No bridge modules or management API."""

import argparse
import asyncio
from collections import Counter
from hashlib import sha256
from importlib.metadata import version
import json
from pathlib import Path
import platform
import tempfile

from websockets.asyncio.server import serve
from websockets.exceptions import ConnectionClosed

from peer import Peer16, Peer201

ROOT = Path(__file__).resolve().parent
PACKAGES = ("ocpp", "websockets", "jsonschema", "jsonschema-specifications",
            "referencing", "rpds-py", "attrs")
EXPECTED = {
    "1.6": ["BootNotification", "Heartbeat", "Authorize", "StatusNotification",
            "RemoteStartTransaction:Accepted", "StartTransaction", "MeterValues",
            "RemoteStopTransaction:Accepted", "StopTransaction"],
    "2.0.1": ["BootNotification", "Heartbeat", "Authorize", "StatusNotification",
              "RequestStartTransaction:Accepted", "TransactionEvent:Started",
              "TransactionEvent:Updated", "RequestStopTransaction:Accepted",
              "TransactionEvent:Ended"],
}


def digest(path):
    return sha256(path.read_bytes()).hexdigest()


def validate_result(returncode, output, observed, protocol):
    events = [json.loads(line) for line in output.splitlines()]
    if returncode != 0 or not events or events[-1].get("event") != "run_passed":
        return False
    return Counter(observed) == Counter(EXPECTED[protocol])


async def run_case(binary, protocol, mismatch, output_dir):
    peers = []
    errors = []

    async def connected(connection):
        if connection.request.path != "/alpha" or peers:
            await connection.close(code=1008)
            return
        peer_type = Peer16 if protocol == "1.6" else Peer201
        peer = peer_type("alpha", connection, response_timeout=5)
        peer.initialize(mismatch)
        peers.append(peer)
        commands = asyncio.create_task(peer.remote_commands())
        try:
            await peer.start()
        except ConnectionClosed:
            pass
        except Exception as error:
            errors.append(type(error).__name__)
        finally:
            commands.cancel()
            try:
                await commands
            except asyncio.CancelledError:
                pass
            except Exception as error:
                errors.append(type(error).__name__)

    label = protocol + ("-mismatch" if mismatch else "")
    scenario = ROOT / "scenarios" / f"{protocol}.toml"
    async with serve(connected, "127.0.0.1", 0,
                     subprotocols=["ocpp" + protocol], max_size=65536,
                     max_queue=8, close_timeout=1) as server:
        port = server.sockets[0].getsockname()[1]
        with tempfile.TemporaryDirectory(prefix="uob-external-") as directory:
            config = Path(directory) / "station.toml"
            config.write_text(
                'schema_version = 1\nstation_capacity = 1\n[[stations]]\n'
                'id = "alpha"\n'
                f'endpoint = "ws://127.0.0.1:{port}/alpha"\n'
                f'ocpp_version = "{protocol}"\n'
                'request_timeout_ms = 5000\nreconnect = false\n'
                'command_capacity = 8\ntrace_capacity = 64\nstep_capacity = 32\n'
                + ('connectors = [1]\n' if protocol == "1.6" else
                   '[[stations.evses]]\nid = 2\nconnectors = [1]\n')
            )
            process = await asyncio.create_subprocess_exec(
                str(binary), "run", "--config", str(config),
                "--scenario", str(scenario), "--seed", "171", "--format", "jsonl",
                cwd=directory, stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
            )
            timed_out = False
            try:
                stdout, stderr = await asyncio.wait_for(process.communicate(), 45)
            except TimeoutError:
                timed_out = True
                process.kill()
                stdout, stderr = await process.communicate()
            finally:
                if process.returncode is None:
                    process.kill()
                    await process.wait()
    (output_dir / f"{label}.jsonl").write_bytes(stdout)
    (output_dir / f"{label}.stderr").write_bytes(stderr)
    observed = peers[0].observed if peers else []
    try:
        passed = validate_result(process.returncode, stdout, observed, protocol)
        events = [json.loads(line) for line in stdout.splitlines()]
        failure_code = events[-1].get("failure_code") if events else None
    except (ValueError, KeyError, TypeError):
        passed, failure_code, events = False, "invalid_report", []
    return {
        "protocol": protocol, "case": "rejected_boot" if mismatch else "smoke",
        "status": "passed" if passed and not errors and not timed_out else "failed",
        "simulator_exit_code": process.returncode, "failure_code": failure_code,
        "timed_out": timed_out, "peer_errors": errors, "observed": observed,
        "scenario_sha256": digest(scenario), "seed": 171,
        "simulator_events": events,
    }


async def main(args):
    args.output.mkdir(parents=True, exist_ok=False)
    installed = {package: version(package) for package in PACKAGES}
    # Refuse unnoticed dependency drift, including transitive schema validators.
    for line in (ROOT / "requirements.txt").read_text().splitlines():
        if "==" in line and not line.startswith(" "):
            package, pin = line.split("==", 1)
            if installed[package] != pin.split()[0]:
                raise ValueError("external dependency version mismatch")
    report = {
        "schema_version": 1, "external_implementation": "mobilityhouse/ocpp",
        "dependencies": installed, "python": platform.python_version(),
        "platform": platform.platform(), "simulator_sha256": digest(args.simulator),
        "harness_sha256": {p.name: digest(p) for p in
                           [ROOT / "peer.py", ROOT / "run.py", ROOT / "requirements.txt"]},
        "limitations": ["bounded test CSMS policy, not a complete production CSMS",
                        "loopback plaintext, no authentication/TLS qualification",
                        "no certification, reconnect, persistence or full feature claim"],
        "cases": [],
    }
    for protocol in ("1.6", "2.0.1"):
        report["cases"].append(await run_case(
            args.simulator, protocol, args.inject_mismatch, args.output,
        ))
    report["status"] = ("passed" if all(case["status"] == "passed"
                                        for case in report["cases"]) else "failed")
    (args.output / "evidence.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report))
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--simulator", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--inject-mismatch", action="store_true")
    arguments = parser.parse_args()
    arguments.simulator = arguments.simulator.resolve(strict=True)
    arguments.output = arguments.output.resolve()
    raise SystemExit(asyncio.run(main(arguments)))
