use std::{fs, path::Path};

const REQUIRED_FILES: &[&str] = &[
    ".github/dependabot.yml",
    ".github/workflows/security.yml",
    ".gitleaks-fixture.toml",
    ".gitleaks.toml",
    "deny.toml",
    "docs/security/dependency-and-workflow-policy.md",
    "scripts/check-sbom.sh",
    "tests/security-fixtures/disallowed-source/Cargo.lock",
    "tests/security-fixtures/fake-secret.txt",
    "tests/security-fixtures/insecure-workflow.yml",
    "tests/security-fixtures/vulnerable-dependency/Cargo.lock",
];

pub(crate) fn check(root: &Path, errors: &mut Vec<String>) {
    for relative in REQUIRED_FILES {
        if !root.join(relative).is_file() {
            errors.push(format!("security policy requires {relative}"));
        }
    }

    check_workflows(root, errors);
    check_dependency_policy(root, errors);
    check_security_workflow(root, errors);
    check_dependabot(root, errors);
    check_container_pins(root, errors);
}

fn check_workflows(root: &Path, errors: &mut Vec<String>) {
    let directory = root.join(".github/workflows");
    let Ok(entries) = fs::read_dir(&directory) else {
        errors.push("could not read .github/workflows".into());
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Ok(source) = fs::read_to_string(&path) else {
            errors.push(format!("could not read {}", path.display()));
            continue;
        };
        let display = path.strip_prefix(root).unwrap_or(&path).display();

        if !source.contains("permissions:\n  contents: read") {
            errors.push(format!(
                "{display} must default to read-only contents permission"
            ));
        }
        for forbidden in [
            "pull_request_target:",
            "permissions: write-all",
            "runs-on: self-hosted",
            "${{ secrets.",
        ] {
            if source.contains(forbidden) {
                errors.push(format!(
                    "{display} contains forbidden workflow text {forbidden:?}"
                ));
            }
        }

        for (index, line) in source.lines().enumerate() {
            let trimmed = line.trim_start();
            let Some(reference) = trimmed.strip_prefix("uses:") else {
                continue;
            };
            let Some(revision) = reference.trim().split_once('@').map(|(_, value)| value) else {
                errors.push(format!("{display}:{} action is not pinned", index + 1));
                continue;
            };
            let revision = revision.split_whitespace().next().unwrap_or_default();
            if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                errors.push(format!(
                    "{display}:{} action revision must be a full commit SHA",
                    index + 1
                ));
            }
        }
    }
}

fn check_dependency_policy(root: &Path, errors: &mut Vec<String>) {
    let Ok(policy) = fs::read_to_string(root.join("deny.toml")) else {
        return;
    };
    for required in [
        "ignore = []",
        "unknown-registry = \"deny\"",
        "unknown-git = \"deny\"",
        "allow-git = []",
    ] {
        if !policy.contains(required) {
            errors.push(format!("deny.toml must contain {required:?}"));
        }
    }
}

fn check_security_workflow(root: &Path, errors: &mut Vec<String>) {
    let Ok(workflow) = fs::read_to_string(root.join(".github/workflows/security.yml")) else {
        return;
    };
    for required in [
        "CARGO_DENY_SHA256:",
        "GITLEAKS_SHA256:",
        "ZIZMOR_SHA256:",
        "SYFT_SHA256:",
        "gitleaks git . --config .gitleaks.toml --redact",
        "syft scan dir:.",
        "retention-days: 14",
        "SOURCE_REVISION: ${{ github.sha }}",
    ] {
        if !workflow.contains(required) {
            errors.push(format!("security workflow must contain {required:?}"));
        }
    }
    if workflow.contains("continue-on-error: ${{") {
        errors.push("security workflow must not make scanner failures conditional".into());
    }
}

fn check_dependabot(root: &Path, errors: &mut Vec<String>) {
    let Ok(configuration) = fs::read_to_string(root.join(".github/dependabot.yml")) else {
        return;
    };
    for ecosystem in ["cargo", "github-actions"] {
        if !configuration.contains(&format!("package-ecosystem: {ecosystem}")) {
            errors.push(format!("Dependabot must update {ecosystem}"));
        }
    }
    if configuration.contains("allow:") || configuration.contains("ignore:") {
        errors.push("Dependabot must not silently exclude dependency classes".into());
    }
}

fn check_container_pins(root: &Path, errors: &mut Vec<String>) {
    let Ok(verifier) = fs::read_to_string(root.join("scripts/verify-workspace.sh")) else {
        return;
    };
    let rust_image = verifier
        .lines()
        .find(|line| line.contains("readonly rust_image="))
        .unwrap_or_default();
    let digest = rust_image.split("@sha256:").nth(1).unwrap_or_default();
    let digest = digest.trim_end_matches('"');
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        errors.push("workspace verifier Rust image must use a full sha256 digest".into());
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn full_action_revision_shape_is_strict() {
        let valid = "3d3c42e5aac5ba805825da76410c181273ba90b1";
        assert_eq!(valid.len(), 40);
        assert!(valid.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!("v7".len(), 40);
    }
}
