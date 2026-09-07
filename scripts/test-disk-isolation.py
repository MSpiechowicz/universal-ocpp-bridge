#!/usr/bin/env python3
"""Privileged, disposable mount-namespace test; never use on a charging host."""
import errno
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def run(*command):
    subprocess.run(command, check=True)


def test():
    spec = importlib.util.spec_from_file_location('disk', ROOT / 'packaging/storage/disk_preflight.py')
    disk = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(disk)
    # Small fixed ext4 images exercise kernel exhaustion without requiring a spare
    # physical disk. Production deliberately rejects loop devices; only this test
    # substitutes the topology observation and scales its free-space threshold.
    disk.HEADROOM = 1024 * 1024
    with tempfile.TemporaryDirectory(prefix='uob-disk-test-') as temporary:
        root = Path(temporary)
        policy = {}
        mounted = []
        try:
            for name in ('production', 'staging', 'artifacts'):
                image = root / (name + '.img')
                with image.open('wb') as stream:
                    os.posix_fallocate(stream.fileno(), 0, 32 * 1024 * 1024)
                run('mkfs.ext4', '-q', '-F', str(image))
                mount = root / name
                mount.mkdir()
                run('mount', '-o', 'loop,nodev,nosuid,noexec', str(image), str(mount))
                mounted.append(mount)
                policy[name] = mount

            def observe(path):
                fs = os.statvfs(path)
                return path.stat().st_dev, fs.f_bavail * fs.f_frsize, fs.f_favail

            disk.check(policy, observer=observe)
            # The real production probe must reject these loop-backed fixtures.
            try:
                disk.check(policy)
            except ValueError:
                pass
            else:
                raise AssertionError('loop devices unexpectedly admitted')
            sentinel = policy['artifacts'] / 'previous-good'
            sentinel.write_bytes(b'keep')
            before = observe(policy['production'])
            with (policy['staging'] / 'export-spool').open('wb', buffering=0) as stream:
                while True:
                    try:
                        stream.write(bytes(1024 * 1024))
                    except OSError as error:
                        assert error.errno == errno.ENOSPC
                        break
            assert observe(policy['production']) == before
            try:
                disk.reserve(policy, 4096, 4096, 4096, observe)
            except ValueError:
                pass
            else:
                raise AssertionError('full staging was admitted')
            assert not (policy['artifacts'] / 'install-reservation').exists()
            assert sentinel.read_bytes() == b'keep'
            print('real ext4 staging exhaustion preserves production capacity and retained artifacts')
        finally:
            for mount in reversed(mounted):
                run('umount', str(mount))


if __name__ == '__main__':
    if sys.argv[1:] == ['--disposable']:
        command = ['unshare', '--mount', '--propagation', 'private',
                   sys.executable, str(Path(__file__).resolve()), '--inside-namespace']
        if os.geteuid() != 0:
            command.insert(0, 'sudo')
        run(*command)
    elif sys.argv[1:] == ['--inside-namespace'] and os.geteuid() == 0:
        test()
    else:
        sys.exit('requires --disposable on an isolated Linux CI host with root/sudo')
