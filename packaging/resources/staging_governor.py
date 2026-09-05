#!/usr/bin/env python3
"""Root-owned staging admission and shedding helper; never controls production."""
import argparse
import dataclasses
import http.client
import json
import os
from pathlib import Path
import socket
import subprocess
import time

MIB = 1024 * 1024
SLICE = 'uob-staging.slice'
CGROUP = Path('/sys/fs/cgroup/uob.slice/uob-staging.slice')


@dataclasses.dataclass(frozen=True)
class Policy:
    start_mib: int = 512
    stop_mib: int = 256
    persistence_seconds: int = 30
    memory_high_mib: int = 384
    memory_max_mib: int = 512
    cpu_percent: int = 50
    cpu_weight: int = 25
    io_weight: int = 25
    production_port: int = 8080
    latency_ms: int = 100
    production_rss_mib: int = 128
    cohosted: bool = True

    @classmethod
    def load(cls, path):
        values = json.loads(path.read_text())
        policy = cls(**values)
        for field in dataclasses.fields(policy):
            value = getattr(policy, field.name)
            if field.name == 'cohosted':
                if type(value) is not bool:
                    raise ValueError('cohosted must be boolean')
            elif type(value) is not int or not 1 <= value <= 1048576:
                raise ValueError('policy integers must be positive and bounded')
        if not (policy.stop_mib < policy.start_mib
                and policy.memory_high_mib < policy.memory_max_mib
                and policy.cpu_percent <= 50
                and policy.cpu_weight < 100 and policy.io_weight < 100
                and policy.production_port <= 65535):
            raise ValueError('invalid staging policy ordering or CPU/IO limits')
        return policy


def audit(event, **fields):
    print(json.dumps({'event': event, **fields}, sort_keys=True), flush=True)


def read_bounded(path):
    with path.open() as stream:
        value = stream.read(65537)
    if len(value) > 65536:
        raise ValueError('oversized kernel observation')
    return value


def number(path):
    value = int(read_bounded(path).strip())
    if value < 0:
        raise ValueError('negative kernel counter')
    return value


def counters(path):
    return {key: int(value) for key, value in
            (line.split() for line in read_bounded(path).splitlines())}


def available_memory(path=Path('/proc/meminfo')):
    for line in read_bounded(path).splitlines():
        words = line.split()
        if words[0] == 'MemAvailable:' and words[2:] == ['kB']:
            value = int(words[1])
            if value >= 0:
                return value * 1024
    raise ValueError('MemAvailable unavailable')


def verify_limits(policy, root=CGROUP):
    # Read actual kernel controls, not just systemd configuration. Missing v2
    # controllers, ignored directives and unbounded limits all fail closed.
    expected = {
        'memory.high': policy.memory_high_mib * MIB,
        'memory.max': policy.memory_max_mib * MIB,
        'memory.swap.max': 0,
        'cpu.weight': policy.cpu_weight,
    }
    for name, value in expected.items():
        if number(root / name) != value:
            raise ValueError('staging cgroup limit mismatch: ' + name)
    quota, period = map(int, read_bounded(root / 'cpu.max').split())
    if quota <= 0 or period <= 0 or quota * 100 != period * policy.cpu_percent:
        raise ValueError('staging CPU quota mismatch')
    if read_bounded(root / 'io.weight').split() != ['default', str(policy.io_weight)]:
        raise ValueError('staging IO weight mismatch')
    events = counters(root / 'memory.events')
    if not {'oom', 'oom_kill', 'high'} <= events.keys():
        raise ValueError('staging memory events unavailable')
    return events


def production_alarm(policy):
    # Loopback-only, no credentials, no redirects, bounded response. This is the
    # existing read-only health route and does not introduce command authority.
    connection = http.client.HTTPConnection('127.0.0.1', policy.production_port, timeout=1)
    try:
        started = time.monotonic()
        connection.request('GET', '/health', headers={'Connection': 'close'})
        response = connection.getresponse()
        payload = response.read(65537)
        elapsed_ms = (time.monotonic() - started) * 1000
        if response.status != 200 or len(payload) > 65536:
            return True
        health = json.loads(payload)
        return (elapsed_ms > policy.latency_ms
                or health['readiness'] != 'ready'
                or health['core_loop'] != 'ready'
                or health['storage'] != 'safe'
                or not health['accepts_new_sessions']
                or health['local_response_latency']['p95_upper_bound_ms'] > policy.latency_ms
                or health['daemon_process']['rss_bytes'] > policy.production_rss_mib * MIB)
    except (OSError, ValueError, KeyError, TypeError, http.client.HTTPException):
        return True
    finally:
        connection.close()


class Governor:
    def __init__(self, policy, baseline):
        self.policy = policy
        self.baseline = baseline
        self.since = {}

    def observe(self, now, available, alarm, events):
        if any(events[key] > self.baseline[key] for key in ('oom', 'oom_kill')):
            return 'staging_oom'
        conditions = {
            'host_memory_pressure': available < self.policy.stop_mib * MIB,
            'production_alarm': alarm,
            # Repeated MemoryHigh throttling is sustained staging load pressure.
            'staging_memory_pressure': events['high'] > self.baseline['high'],
        }
        self.baseline = events
        for reason, active in conditions.items():
            if not active:
                self.since.pop(reason, None)
            elif reason not in self.since:
                self.since[reason] = now
                audit('pressure_started', reason=reason)
            elif now - self.since[reason] >= self.policy.persistence_seconds:
                return reason
        return None


def notify(message):
    address = os.environ['NOTIFY_SOCKET']
    if address.startswith('@'):
        address = '\0' + address[1:]
    with socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM) as notifier:
        notifier.connect(address)
        notifier.sendall(message)


def run(policy):
    audit('policy', **dataclasses.asdict(policy))
    baseline = verify_limits(policy)
    if available_memory() < policy.start_mib * MIB:
        raise ValueError('insufficient host memory for staging admission')
    if policy.cohosted and production_alarm(policy):
        raise ValueError('production health refuses staging admission')
    governor = Governor(policy, baseline)
    notify(b'READY=1')
    audit('admitted', slice=SLICE)
    while True:
        time.sleep(1)
        notify(b'WATCHDOG=1')
        events = verify_limits(policy)
        reason = governor.observe(time.monotonic(), available_memory(),
                                  policy.cohosted and production_alarm(policy), events)
        if reason:
            raise ValueError(reason)


def stop_staging(reason):
    audit('staging_stop', reason=reason, slice=SLICE)
    # Queue the stop without waiting for our own BindsTo teardown. Stopping the
    # slice stops every contained unit, including independently started peers.
    subprocess.run(['/usr/bin/systemctl', '--no-block', 'stop', SLICE],
                   check=True, timeout=5)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--policy', type=Path, required=True)
    args = parser.parse_args()
    try:
        run(Policy.load(args.policy))
    except (OSError, ValueError, TypeError, KeyError) as error:
        stop_staging(str(error))
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
