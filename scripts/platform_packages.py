#!/usr/bin/env python3
"""Build and audit narrowly allowlisted, architecture-specific runtime archives."""
import argparse
import hashlib
import io
import json
import pathlib
import platform
import subprocess
import tarfile
import tomllib

ROOT = pathlib.Path(__file__).resolve().parents[1]
TARGETS = {"x86_64-unknown-linux-gnu": 62, "aarch64-unknown-linux-gnu": 183}
FILES = {
    "service": {"bin/uob", "LICENSE", "INSTALL.md"},
    "simulator": {"bin/uob-sim", "LICENSE", "INSTALL.md"},
}
MAX_BYTES = 256 * 1024 * 1024


def digest(data):
    return hashlib.sha256(data).hexdigest()


def elf(data, target):
    if (len(data) < 64 or data[:7] != b"\x7fELF\x02\x01\x01"
            or int.from_bytes(data[18:20], "little") != TARGETS[target]):
        raise ValueError("binary is not a Linux ELF64 for the declared target")


def audit(archive, kind, target):
    """Reject unexpected files, links, duplicate paths, wrong machines and corruption."""
    expected = FILES[kind] | {"manifest.json"}
    content = {}
    with tarfile.open(archive, "r:gz") as package:
        total = 0
        for member in package:
            total += member.size
            if (member.name not in expected or member.name in content
                    or not member.isfile() or total > MAX_BYTES
                    or member.mode != (0o755 if member.name.startswith("bin/") else 0o644)):
                raise ValueError("package content policy violation")
            content[member.name] = package.extractfile(member).read()
    if set(content) != expected:
        raise ValueError("missing required package content")
    manifest = json.loads(content.pop("manifest.json"))
    if (manifest["schema_version"] != 1 or manifest["kind"] != kind
            or manifest["target"] != target or manifest["pi_performance"] != "unqualified"
            or manifest["files"] != {name: digest(data) for name, data in content.items()}):
        raise ValueError("manifest/content mismatch")
    # These nonbinary files must be exact reviewed inputs, never arbitrary build-directory copies.
    if content["LICENSE"] != (ROOT / "LICENSE").read_bytes():
        raise ValueError("unexpected license content")
    if content["INSTALL.md"] != (ROOT / "docs/operations/platform-packages.md").read_bytes():
        raise ValueError("unexpected installation content")
    binary = "bin/uob" if kind == "service" else "bin/uob-sim"
    elf(content[binary], target)
    return manifest


def build(binary_dir, target, output):
    if platform.machine() != target.split("-")[0]:
        raise ValueError("this qualification pipeline requires a native build host")
    rustc = (binary_dir / "rustc.txt").read_text()
    if not rustc.startswith("rustc 1.98.0 ") or f"host: {target}\n" not in rustc:
        raise ValueError("build requires pinned Rust 1.98.0 on the declared native target")
    output.mkdir(parents=True, exist_ok=True)
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    source = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    tree = subprocess.check_output(["git", "rev-parse", "HEAD^{tree}"], cwd=ROOT, text=True).strip()
    dirty = bool(subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT))
    for kind in FILES:
        binary = "uob" if kind == "service" else "uob-sim"
        content = {
            f"bin/{binary}": (binary_dir / "release" / binary).read_bytes(),
            "LICENSE": (ROOT / "LICENSE").read_bytes(),
            "INSTALL.md": (ROOT / "docs/operations/platform-packages.md").read_bytes(),
        }
        elf(content[f"bin/{binary}"], target)
        manifest = {
            "schema_version": 1, "kind": kind, "product_version": version,
            "source_revision": source, "source_tree": tree, "dirty_source": dirty,
            "target": target, "rustc": rustc, "build_mode": "native",
            "build_image": "rust:1.98.0-bookworm@sha256:"
                           "82150a52ec202c1b14d7817e14516c392bb7f5cfebd88f1ed531cb37ebd39922",
            "runtime_baseline": "Linux, glibc 2.36 or newer",
            "browser_assets": "embedded" if kind == "service" else "absent",
            "pi_performance": "unqualified",
            "qualification": "package smoke only; not signed activation authorization",
            "files": {name: digest(data) for name, data in content.items()},
        }
        content["manifest.json"] = (json.dumps(manifest, indent=2, sort_keys=True) + "\n").encode()
        archive = output / f"{binary}-{version}-{target}.tar.gz"
        # Exclusive creation prevents a rerun from silently replacing evidence inputs.
        with archive.open("xb") as stream:
            with tarfile.open(fileobj=stream, mode="w:gz") as package:
                for name, data in sorted(content.items()):
                    entry = tarfile.TarInfo(name)
                    entry.size = len(data)
                    entry.mode = 0o755 if name.startswith("bin/") else 0o644
                    package.addfile(entry, io.BytesIO(data))
        audit(archive, kind, target)
        archive.with_suffix(archive.suffix + ".sha256").write_text(
            f"{digest(archive.read_bytes())}  {archive.name}\n")
        print(archive)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary-dir", type=pathlib.Path, required=True)
    parser.add_argument("--target", choices=TARGETS, required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    args = parser.parse_args()
    build(args.binary_dir, args.target, args.output)
