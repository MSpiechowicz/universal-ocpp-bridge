#!/usr/bin/env python3
"""Opt-in ext4 ENOSPC isolation probe; never run on a charging host.

This tests kernel filesystem separation, not Rust export ingestion or the daemon.
"""
import errno
import os
from pathlib import Path
import subprocess
import sys
import tempfile


IMAGE_BYTES = 32 * 1024 * 1024
BLOCK_BYTES = 64 * 1024


def run(*command):
    subprocess.run(command, check=True)


def available(path):
    stat = os.statvfs(path)
    return stat.f_bavail * stat.f_frsize


def probe():
    with tempfile.TemporaryDirectory(prefix='uob-export-spool-isolation-') as temporary:
        root = Path(temporary)
        mounted = []
        try:
            for name in ('operational', 'spool'):
                image = root / (name + '.img')
                with image.open('wb') as file:
                    os.posix_fallocate(file.fileno(), 0, IMAGE_BYTES)
                run('mkfs.ext4', '-q', '-F', str(image))
                mount = root / name
                mount.mkdir()
                run('mount', '-o', 'loop,nodev,nosuid,noexec', str(image), str(mount))
                mounted.append(mount)

            operational, spool = mounted
            if operational.stat().st_dev == spool.stat().st_dev:
                raise AssertionError('operational and spool filesystems share a device')
            marker = operational / 'committed-before-spool-fault'
            with marker.open('wb', buffering=0) as file:
                file.write(b'operational data survives spool exhaustion\n')
                os.fsync(file.fileno())
            before = available(operational)
            if before < BLOCK_BYTES * 2:
                raise AssertionError('operational filesystem has no independent headroom')

            # fsync each write so delayed allocation cannot hide the real ENOSPC.
            payload = bytes(BLOCK_BYTES)
            exhausted = False
            with (spool / 'export-spool-fault').open('wb', buffering=0) as file:
                while True:
                    try:
                        file.write(payload)
                        os.fsync(file.fileno())
                    except OSError as error:
                        if error.errno != errno.ENOSPC:
                            raise
                        exhausted = True
                        break
            if not exhausted:
                raise AssertionError('spool filesystem did not reach ENOSPC')
            if available(operational) != before:
                raise AssertionError('spool exhaustion consumed operational allocation')
            if marker.read_bytes() != b'operational data survives spool exhaustion\n':
                raise AssertionError('operational data was altered')
            after = operational / 'committed-after-spool-fault'
            with after.open('wb', buffering=0) as file:
                file.write(b'operational writes still work\n')
                os.fsync(file.fileno())
            if after.read_bytes() != b'operational writes still work\n':
                raise AssertionError('post-fault operational write was not retained')
        finally:
            for mount in reversed(mounted):
                run('umount', str(mount))
    if root.exists():
        raise AssertionError('disposable ext4 images were not removed')
    print('ext4 spool ENOSPC left operational capacity and durable writes intact; '
          'disposable images removed; Rust export ingestion was not invoked')


if __name__ == '__main__':
    if sys.argv[1:] == ['--disposable']:
        command = ['unshare', '--mount', '--propagation', 'private',
                   sys.executable, str(Path(__file__).resolve()), '--inside-namespace']
        if os.geteuid() != 0:
            command.insert(0, 'sudo')
        run(*command)
    elif sys.argv[1:] == ['--inside-namespace'] and os.geteuid() == 0 \
            and os.stat('/proc/self/ns/mnt').st_ino != os.stat('/proc/1/ns/mnt').st_ino:
        probe()
    else:
        sys.exit('requires --disposable on an isolated Linux CI host with root/sudo; '
                 'a private mount namespace and mkfs.ext4 are required')
