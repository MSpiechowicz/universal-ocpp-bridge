#!/usr/bin/env python3
"""Synthetic histories exercise the real pinned calculator without publishing."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


class Channels(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.repo = self.directory / "repo"
        self.repo.mkdir()
        self.env = dict(os.environ, GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL="/dev/null")
        self.tick = 1700000000
        self.git("init", "--quiet", "--initial-branch=main")
        self.git("config", "user.name", "Channel fixture")
        self.git("config", "user.email", "channel@example.invalid")
        (self.repo / "cog.toml").write_text((ROOT / "cog.toml").read_text())
        (self.repo / "scripts").mkdir()
        hook = self.repo / "scripts/update-workspace-version.sh"
        hook.write_text("#!/bin/sh\ntouch hook-executed\nexit 97\n")
        hook.chmod(0o755)
        self.commit("chore: baseline")
        self.git("tag", "v1.2.3")
        self.git("switch", "--quiet", "-c", "next")
        self.artifact = self.directory / "artifact"
        self.artifact.write_bytes(b"immutable built artifact")
        self.evidence = self.directory / "candidate.json"

    def run_command(self, *args, check=True, env=None):
        return subprocess.run(args, cwd=self.repo, env=env or self.env, text=True,
                              capture_output=True, check=check)

    def git(self, *args):
        return self.run_command("git", *args).stdout.strip()

    def commit(self, message):
        self.tick += 1
        self.git("add", ".")
        self.run_command("git", "commit", "--quiet", "--allow-empty", "-m", message,
                         env=dict(self.env, GIT_AUTHOR_DATE=f"{self.tick} +0000",
                                  GIT_COMMITTER_DATE=f"{self.tick} +0000"))

    def snapshot(self):
        return [self.git(*args) for args in [
            ("rev-parse", "HEAD"), ("symbolic-ref", "HEAD"), ("show-ref",),
            ("status", "--porcelain", "--untracked-files=all"),
            ("diff", "--binary", "HEAD"), ("ls-files", "-s")]]

    def calculate(self, action="candidate", success=True):
        before = self.snapshot()
        command = ["python3", "-B", str(ROOT / "scripts/propose-candidate-channel.py"),
                   action, "--artifact", str(self.artifact)]
        if action == "promote":
            command += ["--candidate", str(self.evidence)]
        result = self.run_command(*command, check=False)
        self.assertEqual(before, self.snapshot())
        self.assertFalse((self.repo / "hook-executed").exists())
        if not success:
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(result.stdout, "")
            return None
        self.assertEqual(result.returncode, 0, result.stderr)
        value = json.loads(result.stdout)
        self.assertFalse(value["publication_authorized"])
        self.assertFalse(value["qualification_verified"])
        return value

    def candidate(self):
        (self.repo / "feature").write_text("reviewed feature\n")
        self.commit("feat: feature")
        result = self.calculate()
        self.evidence.write_text(json.dumps(result))
        self.git("tag", "v" + result["candidate_version"])
        return result

    def test_sequential_rc_and_no_change_rerun(self):
        self.assertEqual(self.calculate()["status"], "no_release")
        first = self.candidate()
        self.assertEqual(first["candidate_version"], "1.3.0-rc.1")
        # Pin the actual upstream behavior which the wrapper must correct.
        self.assertEqual(self.run_command("cog", "bump", "--auto", "--dry-run",
                                          "--pre", "rc.*").stdout.strip(), "v1.3.0-rc.2")
        self.assertEqual(self.calculate()["candidate_version"], first["candidate_version"])
        self.assertEqual(self.calculate()["status"], "existing_candidate")
        self.commit("fix: followup")
        self.assertEqual(self.calculate()["candidate_version"], "1.3.0-rc.2")
        self.git("tag", "v1.3.0-rc.2")
        self.commit("docs: candidate evidence")
        self.assertEqual(self.calculate()["candidate_version"], "1.3.0-rc.3")
        self.assertEqual(self.calculate()["baseline"], "v1.2.3")

    def test_same_candidate_promotion_and_stable_rerun(self):
        first = self.candidate()
        self.git("switch", "--quiet", "main")
        self.git("merge", "--ff-only", "next")
        result = self.calculate("promote")
        self.assertEqual(result["stable_version"], "1.3.0")
        self.assertEqual(result["artifact_digest"], first["artifact_digest"])
        self.assertEqual(result["build_source_revision"], first["source_revision"])
        self.git("tag", "v1.3.0")
        self.assertEqual(self.calculate("promote")["stable_version"], "1.3.0")
        self.git("switch", "--quiet", "next")
        self.commit("fix: next patch")
        self.assertEqual(self.calculate()["candidate_version"], "1.3.1-rc.1")

    def test_merge_preserves_candidate(self):
        self.candidate()
        self.git("switch", "--quiet", "main")
        self.git("merge", "--no-ff", "next", "-m", "Merge candidate channel")
        self.assertEqual(self.calculate("promote")["stable_version"], "1.3.0")

    def test_tree_and_digest_changes_require_new_qualification(self):
        self.candidate()
        self.git("switch", "--quiet", "main")
        self.git("merge", "--ff-only", "next")
        self.artifact.write_bytes(b"rebuilt artifact")
        self.calculate("promote", success=False)
        self.artifact.write_bytes(b"immutable built artifact")
        (self.repo / "feature").write_text("changed source")
        self.commit("fix: changed source")
        self.calculate("promote", success=False)

    def test_squash_loses_ancestry_even_with_same_tree(self):
        first = self.candidate()
        self.git("switch", "--quiet", "main")
        self.git("merge", "--squash", "next")
        self.commit("feat: feature")
        self.assertEqual(self.git("rev-parse", "HEAD^{tree}"), first["source_tree"])
        self.calculate("promote", success=False)

    def test_initial_and_internal_candidates(self):
        self.git("tag", "-d", "v1.2.3")
        self.commit("feat: initial")
        self.assertEqual(self.calculate()["status"], "readiness_required")
        self.git("tag", "v0.28.0", "HEAD~1")
        result = self.calculate()
        self.assertEqual(result["candidate_version"], "0.29.0-rc.1")
        self.assertTrue(result["first_stable_readiness_required"])
        self.evidence.write_text(json.dumps(result))
        self.git("tag", "v0.29.0-rc.1")
        self.git("switch", "--quiet", "main")
        self.git("merge", "--ff-only", "next")
        self.calculate("promote", success=False)

    def test_invalid_state_fails_closed(self):
        self.candidate()
        (self.repo / "untracked").touch()
        self.calculate(success=False)
        (self.repo / "untracked").unlink()
        self.git("switch", "--quiet", "-c", "topic")
        self.calculate(success=False)
        self.git("switch", "--quiet", "next")
        (self.repo / "cog.toml").write_text("invalid = [\n")
        self.commit("fix: invalid config")
        self.calculate(success=False)

    def test_candidate_tag_collision_on_divergent_branch(self):
        self.git("switch", "--quiet", "-c", "divergent")
        self.commit("feat: unrelated")
        self.git("tag", "v1.3.0-rc.1")
        self.git("switch", "--quiet", "next")
        self.commit("feat: candidate")
        self.calculate(success=False)

    def test_evidence_and_tag_must_match(self):
        first = self.candidate()
        self.git("switch", "--quiet", "main")
        self.git("merge", "--ff-only", "next")
        first["source_tree"] = "0" * 40
        self.evidence.write_text(json.dumps(first))
        self.calculate("promote", success=False)
        first["source_tree"] = self.git("rev-parse", "HEAD^{tree}")
        self.evidence.write_text(json.dumps(first))
        self.git("tag", "-f", "v1.3.0-rc.1", "HEAD~1")
        self.calculate("promote", success=False)

    def test_shallow_history_and_wrong_tool(self):
        self.candidate()
        shallow = self.directory / "shallow"
        self.git("clone", "--quiet", "--depth=1", self.repo.as_uri(), str(shallow))
        original = self.repo
        self.repo = shallow
        self.calculate(success=False)
        self.repo = original
        wrong = self.directory / "wrong-cog"
        wrong.write_text("#!/bin/sh\necho 'cog 6.0.0'\n")
        wrong.chmod(0o755)
        self.env["COG_BIN"] = str(wrong)
        self.calculate(success=False)

    def test_feature_gate_rejects_channel_promotion(self):
        gate = str(ROOT / "scripts/check-channel-promotion.sh")
        for head, base, expected in [("feature", "main", 0), ("feature", "next", 0),
                                     ("next", "main", 1)]:
            result = self.run_command(gate, check=False, env=dict(
                self.env, PR_HEAD_BRANCH=head, PR_BASE_BRANCH=base))
            self.assertEqual(result.returncode, expected)


if __name__ == "__main__":
    unittest.main()
