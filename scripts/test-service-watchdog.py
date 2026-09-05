#!/usr/bin/env python3
"""Actual systemd readiness, watchdog, crash restart and cause evidence on disposable CI only."""
import configparser
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import uuid


def run(*args, check=True):
    return subprocess.run(args, check=check, capture_output=True, text=True, timeout=40).stdout.strip()


def state(unit):
    return dict(line.split('=', 1) for line in run(
        'systemctl', 'show', unit, '--property=ActiveState,SubState,MainPID,Result,NRestarts'
    ).splitlines())


def wait_for(unit, predicate, seconds=25):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        current = state(unit)
        if predicate(current):
            return current
        time.sleep(0.05)
    raise AssertionError(f'{unit} did not reach expected state: {state(unit)}')


def main():
    if sys.argv[1:] != ['--disposable']:
        raise SystemExit('requires --disposable on an isolated systemd test host')
    if os.geteuid() != 0:
        os.execvp('sudo', ['sudo', '--', sys.executable, __file__, '--disposable'])
    repository = Path(__file__).resolve().parents[1]
    binary = repository / 'target/debug/uob'
    assert binary.is_file(), 'build the service before this test'
    unit = 'uob-watchdog-test-' + uuid.uuid4().hex + '.service'
    policy = configparser.ConfigParser(interpolation=None, strict=False)
    policy.read(repository / 'packaging/systemd/uob.service')
    with tempfile.TemporaryDirectory(prefix='uob-watchdog-', dir='/run') as directory:
        root = Path(directory)
        for name in ('state', 'runtime'):
            (root / name).mkdir(mode=0o700)
        config = root / 'bridge.toml'
        config.write_text("[bridge]\nid='systemd-watchdog-test'\n[management]\nlisten_addr='127.0.0.1:0'\n")
        properties = []
        for section, keys in [('Unit', ['StartLimitIntervalSec', 'StartLimitBurst']),
                              ('Service', ['Type', 'NotifyAccess', 'TimeoutStartSec', 'WatchdogSec',
                                           'TimeoutAbortSec', 'LimitCORE', 'Restart', 'RestartSec',
                                           'ExecStopPost'])]:
            for key in keys:
                properties.append('--property=' + key + '=' + policy[section][key])
        environment = ['--setenv=UOB_DEPLOYMENT_ENVIRONMENT=production',
                       '--setenv=STATE_DIRECTORY=' + str(root / 'state'),
                       '--setenv=RUNTIME_DIRECTORY=' + str(root / 'runtime')]
        try:
            # systemd-run waits for READY=1 because Type=notify is copied from packaging.
            run('systemd-run', '--unit=' + unit, *properties, *environment,
                str(binary), 'serve', '--no-ui', '--config', str(config))
            first = wait_for(unit, lambda s: s['SubState'] == 'running')
            # More than one watchdog period proves fresh worker acknowledgements.
            time.sleep(11)
            assert state(unit)['MainPID'] == first['MainPID']
            assert state(unit)['NRestarts'] == '0'
            os.kill(int(first['MainPID']), signal.SIGSTOP)
            restarted = wait_for(unit, lambda s: s['SubState'] == 'running'
                                 and s['MainPID'] != first['MainPID'])
            # Four rapid crashes after the watchdog restart exhaust the shipped
            # five-start/60-second policy, without touching any real UOB unit.
            for index in range(4):
                previous = restarted['MainPID']
                os.kill(int(previous), signal.SIGKILL)
                if index < 3:
                    restarted = wait_for(unit, lambda s: s['SubState'] == 'running'
                                         and s['MainPID'] != previous)
            wait_for(unit, lambda s: s['Result'] == 'start-limit-hit')
            run('journalctl', '--sync')
            journal = run('journalctl', '--unit=' + unit, '--output=cat', '--no-pager')
            assert 'uob_service_exit result=watchdog' in journal, journal
            assert 'uob_service_exit result=signal code=killed status=KILL invocation=' in journal, journal
            print('systemd readiness, stalled-process watchdog, bounded crash restarts and causes passed')
        finally:
            run('systemctl', 'stop', unit, check=False)
            run('systemctl', 'reset-failed', unit, check=False)


if __name__ == '__main__':
    main()
