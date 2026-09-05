#!/usr/bin/env python3
"""Deterministic admission, timing, kernel-control and production alarm tests."""
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

SOURCE = Path(__file__).resolve().parents[1] / 'packaging/resources/staging_governor.py'
SPEC = importlib.util.spec_from_file_location('staging_governor', SOURCE)
g = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = g
SPEC.loader.exec_module(g)


class GovernorTests(unittest.TestCase):
    def setUp(self):
        self.policy = g.Policy()
        self.events = {'oom': 0, 'oom_kill': 0, 'high': 0}
        self.governor = g.Governor(self.policy, dict(self.events))

    def observe(self, now, memory=512, alarm=False):
        return self.governor.observe(now, memory * g.MIB, alarm, dict(self.events))

    def test_memory_threshold_and_continuous_duration(self):
        self.assertIsNone(self.observe(0, 256))
        self.assertIsNone(self.observe(1, 255))
        self.assertIsNone(self.observe(30.99, 255))
        self.assertEqual(self.observe(31, 255), 'host_memory_pressure')

    def test_recovery_resets_duration_and_alarms_are_independent(self):
        self.observe(0, 255)
        self.observe(29, 256)
        self.observe(30, 255, True)
        self.assertIsNone(self.observe(59, 512, True))
        self.assertEqual(self.observe(60, 512, True), 'production_alarm')

    def test_oom_in_any_descendant_sheds_immediately(self):
        self.events['oom_kill'] = 1
        self.assertEqual(self.observe(0), 'staging_oom')

    def test_repeated_high_pressure_sheds_load_storm(self):
        for second in range(31):
            self.events['high'] += 1
            result = self.observe(second)
        self.assertEqual(result, 'staging_memory_pressure')

    def test_admission_refuses_low_memory_without_ready(self):
        with patch.object(g, 'verify_limits', return_value=self.events), \
             patch.object(g, 'available_memory', return_value=511 * g.MIB), \
             patch.object(g, 'notify') as ready:
            with self.assertRaisesRegex(ValueError, 'insufficient'):
                g.run(self.policy)
            ready.assert_not_called()

    def test_admission_requires_healthy_production(self):
        with patch.object(g, 'verify_limits', return_value=self.events), \
             patch.object(g, 'available_memory', return_value=512 * g.MIB), \
             patch.object(g, 'production_alarm', return_value=True), \
             patch.object(g, 'notify') as ready:
            with self.assertRaisesRegex(ValueError, 'production health'):
                g.run(self.policy)
            ready.assert_not_called()

    def test_kernel_limits_missing_or_unbounded_refuse(self):
        files = {'memory.high': str(384 * g.MIB), 'memory.max': str(512 * g.MIB),
                 'memory.swap.max': '0',
                 'cpu.weight': '25', 'cpu.max': '50000 100000',
                 'io.weight': 'default 25', 'memory.events': 'oom 0\noom_kill 0\nhigh 0\n'}
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.assertRaises(OSError):
                g.verify_limits(self.policy, root)
            for name, value in files.items():
                (root / name).write_text(value)
            self.assertEqual(g.verify_limits(self.policy, root), self.events)
            for name in files:
                original = files[name]
                (root / name).write_text('max')
                with self.subTest(name=name), self.assertRaises((ValueError, OSError)):
                    g.verify_limits(self.policy, root)
                (root / name).write_text(original)

    def test_memavailable_is_required_and_parsed_as_kib(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'meminfo'
            path.write_text('MemFree: 900000 kB\n')
            with self.assertRaises(ValueError):
                g.available_memory(path)
            path.write_text('MemAvailable: 524288 kB\n')
            self.assertEqual(g.available_memory(path), 512 * g.MIB)

    def test_policy_rejects_unknown_invalid_and_inconsistent_fields(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'policy'
            for values in ({'typo': 1}, {'start_mib': False}, {'stop_mib': 600},
                           {'cpu_percent': 100}, {'cohosted': 'false'},
                           {'persistence_seconds': 0}, {'io_weight': 100}):
                path.write_text(json.dumps(values))
                with self.subTest(values=values), self.assertRaises((ValueError, TypeError)):
                    g.Policy.load(path)

    def test_health_is_bounded_readonly_and_fail_closed(self):
        healthy = {'readiness': 'ready', 'core_loop': 'ready', 'storage': 'safe',
                   'accepts_new_sessions': True,
                   'local_response_latency': {'p95_upper_bound_ms': 100},
                   'daemon_process': {'rss_bytes': 128 * g.MIB}}
        with patch.object(g.http.client, 'HTTPConnection') as factory:
            connection = factory.return_value
            response = connection.getresponse.return_value
            response.status = 200
            response.read.return_value = json.dumps(healthy).encode()
            self.assertFalse(g.production_alarm(self.policy))
            connection.request.assert_called_with('GET', '/health', headers={'Connection': 'close'})
            response.read.assert_called_with(65537)
            healthy['local_response_latency']['p95_upper_bound_ms'] = 101
            response.read.return_value = json.dumps(healthy).encode()
            self.assertTrue(g.production_alarm(self.policy))
            for payload in (b'{}', b'bad JSON', b'x' * 65537):
                response.read.return_value = payload
                self.assertTrue(g.production_alarm(self.policy))
            response.status = 503
            self.assertTrue(g.production_alarm(self.policy))
            connection.request.side_effect = TimeoutError()
            self.assertTrue(g.production_alarm(self.policy))

    def test_stop_only_targets_staging_and_is_observable(self):
        with patch.object(g.subprocess, 'run') as run, patch.object(g, 'audit') as audit:
            g.stop_staging('staging_oom')
            run.assert_called_once_with(
                ['/usr/bin/systemctl', '--no-block', 'stop', 'uob-staging.slice'],
                check=True, timeout=5)
            audit.assert_called_once_with('staging_stop', reason='staging_oom',
                                          slice='uob-staging.slice')


if __name__ == '__main__':
    unittest.main()
