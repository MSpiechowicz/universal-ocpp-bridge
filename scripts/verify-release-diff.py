#!/usr/bin/env python3
"""Allow only workspace version metadata and changelog in an automatic release."""
import subprocess
import sys
import tomllib

base, release = sys.argv[1:]


def git(*args):
    return subprocess.check_output(['git', *args])


def document(revision, path):
    return tomllib.loads(git('show', f'{revision}:{path}').decode())


changed = set(git('diff', '--name-only', base, release).decode().splitlines())
if not changed <= {'Cargo.toml', 'Cargo.lock', 'CHANGELOG.md'}:
    sys.exit('Release may change only Cargo.toml, Cargo.lock, and CHANGELOG.md')
before = document(base, 'Cargo.toml')
after = document(release, 'Cargo.toml')
old = before['workspace']['package']['version']
new = after['workspace']['package']['version']
after['workspace']['package']['version'] = old
if before != after:
    sys.exit('Release modified manifest fields other than workspace version')
if tuple(map(int, new.split('.'))) <= tuple(map(int, old.split('.'))):
    sys.exit('Release version must increase')
before_lock = document(base, 'Cargo.lock')
after_lock = document(release, 'Cargo.lock')
# Normalize only local packages that followed the old workspace version.
for package in after_lock['package']:
    if 'source' not in package and package['version'] == new:
        original = next((p for p in before_lock['package']
                         if p['name'] == package['name'] and 'source' not in p), None)
        if original and original['version'] == old:
            package['version'] = old
if before_lock != after_lock:
    sys.exit('Release modified locked dependencies or independent package versions')
# Git file modes are part of the release boundary too.
if git('diff', '--summary', base, release).strip():
    sys.exit('Release must not create, delete, rename, or change file modes')
