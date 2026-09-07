#!/usr/bin/env python3
"""Exercise admission and real preallocation without changing host partitions."""
import errno
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('disk', ROOT / 'packaging/storage/disk_preflight.py')
disk = importlib.util.module_from_spec(spec)
spec.loader.exec_module(disk)


class Admission(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.policy = {name: self.root / name for name in ('production', 'staging', 'artifacts')}
        for root in self.policy.values():
            root.mkdir(mode=0o700)
        self.observations = {root: (index, 2 * disk.HEADROOM, 100)
                             for index, root in enumerate(self.policy.values())}

    def observe(self, root):
        return self.observations[root]

    def reserve(self, count=4096):
        return disk.reserve(self.policy, count, count, count, self.observe)

    def test_each_budget_and_exact_boundary(self):
        for name, root in self.policy.items():
            with self.subTest(name=name):
                device, _, _ = self.observations[root]
                extra = 3 * 4096 if name == 'artifacts' else 0
                self.observations[root] = device, disk.HEADROOM + extra, 16
                disk.check(self.policy, 4096, 4096, 4096, self.observe)
                self.observations[root] = device, disk.HEADROOM + extra - 1, 16
                with self.assertRaisesRegex(ValueError, name + ': insufficient space'):
                    self.reserve()
                self.assertFalse((self.policy['artifacts'] / 'install-reservation').exists())
                self.observations[root] = device, 2 * disk.HEADROOM, 100

    def test_shared_filesystem_and_inode_exhaustion(self):
        staging = self.policy['staging']
        self.observations[staging] = self.observations[self.policy['production']]
        with self.assertRaisesRegex(ValueError, 'separate partitions'):
            self.reserve()
        self.observations[staging] = 1, 2 * disk.HEADROOM, 0
        with self.assertRaisesRegex(ValueError, 'free_inodes=0'):
            self.reserve()

    def test_real_blocks_are_reserved_and_existing_artifacts_untouched(self):
        artifacts = self.policy['artifacts']
        for name in ('active', 'previous-good', 'activation.json', 'unrelated.tmp'):
            (artifacts / name).write_bytes(b'retained')
        result = self.reserve()
        for name in ('candidate', 'previous', 'backup'):
            info = (result / name).stat()
            self.assertEqual(info.st_size, 4096)
            self.assertGreaterEqual(info.st_blocks * 512, 4096)
        with self.assertRaises(FileExistsError):
            self.reserve()
        for name in ('active', 'previous-good', 'activation.json', 'unrelated.tmp'):
            self.assertEqual((artifacts / name).read_bytes(), b'retained')

    def test_allocation_failure_cleans_only_this_invocations_files(self):
        artifact = self.policy['artifacts'] / 'previous-good'
        artifact.write_bytes(b'keep')
        with patch.object(os, 'posix_fallocate', side_effect=OSError(errno.ENOSPC, 'disk full')):
            with self.assertRaises(OSError):
                self.reserve()
        self.assertFalse((artifact.parent / 'install-reservation').exists())
        self.assertEqual(artifact.read_bytes(), b'keep')

    def test_postallocation_headroom_loss_cancels_ticket(self):
        calls = 0

        def observe(root):
            nonlocal calls
            calls += 1
            device, available, inodes = self.observations[root]
            return device, (0 if calls > 3 else available), inodes

        with self.assertRaisesRegex(ValueError, 'insufficient space'):
            disk.reserve(self.policy, 4096, 4096, 4096, observe)
        self.assertFalse((self.policy['artifacts'] / 'install-reservation').exists())

    def test_invalid_sizes_symlink_and_unprotected_artifact_root(self):
        for size in (0, -1, True, 1.2, disk.LIMIT + 1):
            with self.subTest(size=size), self.assertRaises(ValueError):
                self.reserve(size)
        artifact = self.policy['artifacts']
        (artifact / '.disk-admission.lock').symlink_to(self.root / 'victim')
        with self.assertRaises(OSError):
            self.reserve()
        self.assertFalse((self.root / 'victim').exists())
        artifact.chmod(0o777)
        with self.assertRaisesRegex(ValueError, 'not group/world writable'):
            self.reserve()

    def test_unprovisioned_host_directory_fails_closed(self):
        with self.assertRaises(ValueError):
            disk.observe(self.root)

    def test_policy_is_bounded_and_exact(self):
        policy = self.root / 'policy.json'
        for contents in ('{}', '{"production": "relative"}', ' ' * 16385):
            policy.write_text(contents)
            with self.assertRaises(ValueError):
                disk.load_policy(policy)

    def test_staging_has_mandatory_disk_admission(self):
        unit = (ROOT / 'packaging/systemd/uob-staging.service').read_text()
        self.assertIn('Requires=uob-staging-disk.service', unit)
        self.assertIn('After=uob-staging-disk.service', unit)
        check = (ROOT / 'packaging/systemd/uob-staging-disk.service').read_text()
        self.assertIn('disk_preflight.py --policy /etc/uob-staging/disk-policy.json check', check)
        self.assertNotIn('RemainAfterExit=yes', check)


if __name__ == '__main__':
    unittest.main()
