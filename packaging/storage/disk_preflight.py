#!/usr/bin/env python3
"""Fail-closed disk admission and allocation for administrator-owned release work."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import stat
import sys

MIB = 1024 * 1024
HEADROOM = 512 * MIB
LIMIT = 1024 ** 4


def size(value):
    if type(value) is not int or not 0 < value <= LIMIT:
        raise ValueError('sizes must be positive byte counts no larger than 1 TiB')
    return value


def load_policy(path):
    with path.open() as stream:
        encoded = stream.read(16385)
    if len(encoded) > 16384:
        raise ValueError('disk policy exceeds 16 KiB')
    policy = json.loads(encoded)
    if set(policy) != {'production', 'staging', 'artifacts'}:
        raise ValueError('policy requires production, staging and artifacts mount roots')
    for value in policy.values():
        if not isinstance(value, str) or not Path(value).is_absolute():
            raise ValueError('mount roots must be absolute paths')
    return {key: Path(value) for key, value in policy.items()}


def observe(root):
    # Restrict the supported deployment to fixed, thick disk partitions. Different
    # st_dev alone is not enough: sparse loop images or thin LVs can share a pool.
    if root.resolve(strict=True) != root or not root.is_dir():
        raise ValueError(f'{root}: mount root must be canonical, without symlinks')
    info = root.stat()
    device = f'{os.major(info.st_dev)}:{os.minor(info.st_dev)}'
    block = Path('/sys/dev/block') / device
    if (not (block / 'partition').is_file()
            or '/virtual/' in str(block.resolve())):
        raise ValueError(f'{root}: requires a dedicated fixed disk partition; '
                         'directory, loop, thin, network and virtual filesystems are unsupported')
    if root.parent.stat().st_dev == info.st_dev:
        raise ValueError(f'{root}: expected the partition mount root')
    # Reject multi-device/COW filesystems whose space accounting can share pools.
    with Path('/proc/self/mountinfo').open() as stream:
        mounts = stream.read(1024 * 1024 + 1)
    if len(mounts) > 1024 * 1024:
        raise ValueError('mount table exceeds bound')
    matches = [line.split(' - ', 1)[1].split()[0]
               for line in mounts.splitlines()
               if line.split()[2] == device and line.split()[3] == '/']
    if not matches or any(kind != 'ext4' for kind in matches):
        raise ValueError(f'{root}: requires a dedicated ext4 partition')
    fs = os.statvfs(root)
    if fs.f_flag & os.ST_RDONLY:
        raise ValueError(f'{root}: filesystem is read-only')
    return info.st_dev, fs.f_bavail * fs.f_frsize, fs.f_favail


def check(policy, candidate=0, previous=0, backup=0, observer=observe):
    requested = (candidate, previous, backup)
    if any(requested):
        for value in requested:
            size(value)
    observations = {key: observer(root) for key, root in policy.items()}
    if len({entry[0] for entry in observations.values()}) != 3:
        raise ValueError('production, staging and artifacts must use separate partitions')
    for key, (_, available, inodes) in observations.items():
        required = HEADROOM + (sum(requested) if key == 'artifacts' else 0)
        if available < required or inodes < 16:
            raise ValueError(f'{key}: insufficient space: available={available} '
                             f'required={required} free_inodes={inodes}; '
                             'increase partition capacity or remove reviewed unreferenced '
                             'temporary data; active and previous-good artifacts must be retained')
    return observations


def reserve(policy, candidate, previous, backup, observer=observe):
    """Allocate real blocks before installation can consume its capacity ticket.

    This does not activate an artifact or prune existing files. A single root-owned
    reservation remains until the release manager/operator explicitly consumes or
    cancels it. No stale reservation is automatically considered unreferenced.
    """
    for value in (candidate, previous, backup):
        size(value)
    root = policy['artifacts']
    info = root.stat()
    if info.st_uid != os.geteuid() or stat.S_IMODE(info.st_mode) & 0o022:
        raise ValueError('artifact mount root must be owned by the caller and not group/world writable')
    lock_fd = os.open(root / '.disk-admission.lock',
                      os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600)
    try:
        lock = os.fstat(lock_fd)
        if (lock.st_nlink != 1 or not stat.S_ISREG(lock.st_mode)
                or lock.st_uid != os.geteuid()):
            raise ValueError('invalid admission lock')
        fcntl.flock(lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        check(policy, candidate, previous, backup, observer)
        reservation = root / 'install-reservation'
        reservation.mkdir(mode=0o700)  # Existing/incomplete reservation fails closed.
        created = []
        try:
            for name, count in [('candidate', candidate), ('previous', previous),
                                ('backup', backup)]:
                path = reservation / name
                fd = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY | os.O_NOFOLLOW, 0o600)
                created.append(path)
                try:
                    os.posix_fallocate(fd, 0, count)
                    os.fsync(fd)
                finally:
                    os.close(fd)
            # Recheck actual remaining capacity, including production changes while
            # allocation was in progress. A free-space observation is never a ticket.
            check(policy, observer=observer)
            for path in (reservation, root):
                fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
                try:
                    os.fsync(fd)
                finally:
                    os.close(fd)
            return reservation
        except BaseException:
            # Only this invocation's newly created, private allocation files qualify
            # as unreferenced temporary data. Never scan/prune the artifact store.
            for path in created:
                path.unlink()
            reservation.rmdir()
            raise
    finally:
        os.close(lock_fd)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--policy', type=Path, required=True)
    subcommands = parser.add_subparsers(dest='operation', required=True)
    subcommands.add_parser('check')
    allocation = subcommands.add_parser('reserve')
    for name in ('candidate', 'previous', 'backup'):
        allocation.add_argument('--' + name + '-bytes', type=int, required=True)
    args = parser.parse_args()
    try:
        policy = load_policy(args.policy)
        if args.operation == 'reserve':
            result = reserve(policy, args.candidate_bytes, args.previous_bytes, args.backup_bytes)
            print(json.dumps({'status': 'reserved', 'path': str(result)}))
        else:
            check(policy)
            print(json.dumps({'status': 'admitted', 'headroom_bytes': HEADROOM}))
    except (OSError, ValueError) as error:
        print(json.dumps({'status': 'denied', 'reason': str(error)}), file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
