#!/usr/bin/env python3
"""Negative package boundary fixtures; never used as runtime qualification evidence."""
import io
import json
import pathlib
import tarfile
import tempfile
import unittest

from platform_packages import ROOT, audit, digest

TARGET = "x86_64-unknown-linux-gnu"


class PackagePolicyTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.archive = pathlib.Path(self.directory.name) / "fixture.tar.gz"
        # Minimal ELF header is sufficient for audit unit tests, never a smoke pass.
        binary = bytearray(64)
        binary[:7] = b"\x7fELF\x02\x01\x01"
        binary[18:20] = (62).to_bytes(2, "little")
        self.content = {
            "bin/uob": bytes(binary), "LICENSE": (ROOT / "LICENSE").read_bytes(),
            "INSTALL.md": (ROOT / "docs/operations/platform-packages.md").read_bytes(),
        }

    def write(self, extra=None, mutate_manifest=None):
        manifest = {"schema_version": 1, "kind": "service", "target": TARGET,
                    "pi_performance": "unqualified",
                    "files": {name: digest(data) for name, data in self.content.items()}}
        if mutate_manifest:
            mutate_manifest(manifest)
        with tarfile.open(self.archive, "w:gz") as package:
            for name, data in {**self.content, "manifest.json": json.dumps(manifest).encode()}.items():
                member = tarfile.TarInfo(name)
                member.size = len(data)
                member.mode = 0o755 if name.startswith("bin/") else 0o644
                package.addfile(member, io.BytesIO(data))
            if extra:
                package.addfile(extra, io.BytesIO(b""))

    def test_valid_allowlist_and_both_target_headers(self):
        self.write()
        self.assertEqual(audit(self.archive, "service", TARGET)["kind"], "service")
        data = bytearray(self.content["bin/uob"])
        data[18:20] = (183).to_bytes(2, "little")
        self.content["bin/uob"] = bytes(data)
        target = "aarch64-unknown-linux-gnu"
        self.write(mutate_manifest=lambda manifest: manifest.update(target=target))
        self.assertEqual(audit(self.archive, "service", target)["target"], target)

    def test_simulator_is_a_separate_package(self):
        self.content["bin/uob-sim"] = self.content.pop("bin/uob")
        self.write(mutate_manifest=lambda manifest: manifest.update(kind="simulator"))
        self.assertEqual(audit(self.archive, "simulator", TARGET)["kind"], "simulator")
        with self.assertRaises(ValueError):
            audit(self.archive, "service", TARGET)

    def test_prohibited_runtime_and_secret_paths(self):
        for name in ["bin/uob-sim", "bin/uob-release-manager", "bin/node", "bin/python3",
                     "bin/rustc", "bin/test-provider", "etc/test-token", "../escape"]:
            with self.subTest(name=name):
                self.write(extra=tarfile.TarInfo(name))
                with self.assertRaises(ValueError):
                    audit(self.archive, "service", TARGET)

    def test_duplicates_links_and_missing_files(self):
        self.write(extra=tarfile.TarInfo("LICENSE"))
        with self.assertRaises(ValueError):
            audit(self.archive, "service", TARGET)
        for kind in [tarfile.SYMTYPE, tarfile.LNKTYPE]:
            link = tarfile.TarInfo("LICENSE")
            link.type = kind
            link.linkname = "/etc/passwd"
            self.write(extra=link)
            with self.assertRaises(ValueError):
                audit(self.archive, "service", TARGET)
        del self.content["bin/uob"]
        self.write()
        with self.assertRaises(ValueError):
            audit(self.archive, "service", TARGET)

    def test_hash_corruption_wrong_architecture_and_fabricated_pi_claim(self):
        self.write(mutate_manifest=lambda manifest: manifest["files"].update({"bin/uob": "0" * 64}))
        with self.assertRaises(ValueError):
            audit(self.archive, "service", TARGET)
        self.write(mutate_manifest=lambda manifest: manifest.update(pi_performance="passed"))
        with self.assertRaises(ValueError):
            audit(self.archive, "service", TARGET)
        data = bytearray(self.content["bin/uob"])
        data[18:20] = (183).to_bytes(2, "little")
        self.content["bin/uob"] = bytes(data)
        self.write()
        with self.assertRaises(ValueError):
            audit(self.archive, "service", TARGET)
        self.content["bin/uob"] = b"#!/bin/sh\nexit 0\n"
        self.write()
        with self.assertRaises(ValueError):
            audit(self.archive, "service", TARGET)

    def test_secret_cannot_be_added_to_allowlisted_prose(self):
        self.content["INSTALL.md"] += b"\nfixture-credential=not-a-real-secret\n"
        self.write()
        with self.assertRaises(ValueError):
            audit(self.archive, "service", TARGET)


if __name__ == "__main__":
    unittest.main()
