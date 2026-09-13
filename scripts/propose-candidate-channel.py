#!/usr/bin/env python3
"""Read-only channel calculations; supplied evidence is not a trust/qualification gate."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

VERSION = r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
STABLE = re.compile(r"v" + VERSION)
CANDIDATE = re.compile(r"v" + VERSION + r"-rc\.([1-9][0-9]*)")
NO_RELEASE = (
    "No conventional commits for your repository that required a bump. "
    "Changelogs will be updated on the next bump.\n"
    "Pre-Hooks and Post-Hooks have been skipped."
)


def run(repository, *args):
    return subprocess.run(args, cwd=repository, check=True, text=True,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE).stdout.strip()


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(path):
    value = hashlib.sha256()
    with open(path, "rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            value.update(chunk)
    return "sha256:" + value.hexdigest()


def proposal(args):
    repository = args.repository
    git = lambda *parts: run(repository, "git", *parts)
    cog = lambda *parts: run(repository, os.environ.get("COG_BIN", "cog"), *parts)
    require(cog("--version") == "cog 7.0.0", "Cocogitto 7.0.0 required")
    require(git("rev-parse", "--is-shallow-repository") == "false", "full history required")
    require(not git("status", "--porcelain", "--untracked-files=all"), "clean tree required")
    branch = git("branch", "--show-current")
    require(branch == ("next" if args.action == "candidate" else "main"),
            "candidate requires next; promotion requires main")
    source = git("rev-parse", "HEAD")
    tree = git("rev-parse", "HEAD^{tree}")
    artifact_digest = digest(args.artifact)
    tags = git("tag", "--merged", "HEAD", "--list", "v*").splitlines()
    stable_tags = [tag for tag in tags if STABLE.fullmatch(tag)]
    # Validate the real calculator even for reruns and first-stable decisions.
    calculated = cog("bump", "--auto", "--dry-run")
    require(STABLE.fullmatch(calculated) or calculated == NO_RELEASE,
            "unexpected stable calculation")
    baseline = cog("get-version", "--tag") if stable_tags else None
    require(baseline is None or STABLE.fullmatch(baseline), "invalid stable baseline")
    result = dict(schema_version=1, source_revision=source, source_tree=tree,
                  artifact_digest=artifact_digest, baseline=baseline,
                  publication_authorized=False, qualification_verified=False)
    if args.action == "candidate":
        if baseline is None:
            return dict(result, status="readiness_required", candidate_version=None)
        candidates = [tag for tag in tags if CANDIDATE.fullmatch(tag)]
        at_head = [tag for tag in candidates
                   if git("rev-parse", f"refs/tags/{tag}^{{commit}}") == source]
        require(len(at_head) <= 1, "ambiguous candidate tags at source")
        if at_head:
            version = at_head[0]
            status = "existing_candidate"
        elif (git("rev-parse", f"refs/tags/{baseline}^{{commit}}") == source
              or calculated == NO_RELEASE):
            return dict(result, status="no_release", candidate_version=None)
        else:
            version = cog("bump", "--auto", "--dry-run", "--pre", "rc.*")
            require(CANDIDATE.fullmatch(version), "unexpected candidate calculation")
            require(version.split("-rc.")[0] == calculated, "candidate base differs")
            # A tag on a divergent branch must never be overwritten by a rerun.
            require(version not in git("tag", "--list").splitlines(), "candidate tag already exists")
            status = "proposed"
        return dict(result, status=status, candidate_version=version[1:],
                    first_stable_readiness_required=version.startswith("v0."))

    evidence = json.loads(Path(args.candidate).read_text())
    require(evidence.get("schema_version") == 1, "unsupported candidate evidence")
    version = "v" + str(evidence.get("candidate_version", ""))
    require(CANDIDATE.fullmatch(version), "invalid candidate version")
    candidate_source = evidence.get("source_revision", "")
    require(re.fullmatch(r"[0-9a-f]{40}", candidate_source), "invalid source revision")
    require(git("rev-parse", f"refs/tags/{version}^{{commit}}") == candidate_source,
            "candidate tag does not match evidence")
    git("merge-base", "--is-ancestor", candidate_source, source)
    require(tree == evidence.get("source_tree") == git("rev-parse", f"{candidate_source}^{{tree}}"),
            "source tree changed: new qualification required")
    require(artifact_digest == evidence.get("artifact_digest"),
            "artifact changed: new qualification required")
    stable = version.split("-rc.")[0]
    require(not stable.startswith("v0."), "internal candidate cannot establish first stable readiness")
    require(baseline is not None and not baseline.startswith("v0."),
            "first stable readiness must be established by downstream trusted gates")
    require(calculated == stable or baseline == stable, "stable base changed: new qualification required")
    if stable in git("tag", "--list").splitlines():
        require(git("rev-parse", f"refs/tags/{stable}^{{tree}}") == tree,
                "stable version already belongs to a different tree")
        git("merge-base", "--is-ancestor", candidate_source, f"refs/tags/{stable}^{{commit}}")
    return dict(result, status="promotion_proposed", stable_version=stable[1:],
                candidate_version=version[1:], build_source_revision=candidate_source)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["candidate", "promote"])
    parser.add_argument("--repository", default=".")
    parser.add_argument("--artifact", required=True, help="already-built immutable artifact")
    parser.add_argument("--candidate", help="candidate proposal JSON (promotion only)")
    args = parser.parse_args()
    if args.action == "promote" and not args.candidate:
        parser.error("promote requires --candidate")
    try:
        print(json.dumps(proposal(args), sort_keys=True))
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f"Channel calculation failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
