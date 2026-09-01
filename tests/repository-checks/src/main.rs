use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use serde::Deserialize;

mod documentation;
mod security;

const EXPECTED_PACKAGES: &[&str] = &[
    "uob-application",
    "uob-contracts",
    "uob-database-conformance",
    "uob-domain",
    "uob-external-export-adapter",
    "uob-hostile-websocket-peer",
    "uob-management-adapter",
    "uob-ocpp-fixtures",
    "uob-protocol-adapter",
    "uob-provider-adapter",
    "uob-release-manager",
    "uob-repository-checks",
    "uob-service",
    "uob-sim",
    "uob-storage-adapter",
    "uob-target-adapter",
    "uob-target-conformance",
];

const FORBIDDEN_SOURCE_TERMS: &[&str] = &[
    "rust_ocpp",
    "rustocpp",
    "mqtt",
    "topic",
    "node_id",
    "nodeid",
    "namespace_uri",
    "namespaceuri",
    "register_address",
    "registeraddress",
    "opcua",
    "rumqttc",
    "rusqlite",
    "postgres",
    "axum",
    "tokio::net",
    "websocket",
    "http::",
    "react",
    "vite",
];

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    workspace_members: Vec<String>,
    resolve: Option<Resolve>,
}

#[derive(Deserialize)]
struct Package {
    id: String,
    name: String,
    manifest_path: PathBuf,
    dependencies: Vec<Dependency>,
    targets: Vec<Target>,
}

#[derive(Deserialize)]
struct Dependency {
    name: String,
}

#[derive(Deserialize)]
struct Target {
    name: String,
    kind: Vec<String>,
}

#[derive(Deserialize)]
struct Resolve {
    nodes: Vec<Node>,
}

#[derive(Deserialize)]
struct Node {
    id: String,
    dependencies: Vec<String>,
}

fn main() -> ExitCode {
    let root = env::args_os().nth(1).map_or_else(
        || env::current_dir().expect("current directory"),
        PathBuf::from,
    );

    match check(&root) {
        Ok(()) => {
            println!("repository documentation and Cargo metadata boundaries verified");
            ExitCode::SUCCESS
        }
        Err(errors) => {
            for error in errors {
                eprintln!("repository violation: {error}");
            }
            ExitCode::FAILURE
        }
    }
}

fn check(root: &Path) -> Result<(), Vec<String>> {
    let metadata = load_metadata(root).map_err(|error| vec![error])?;
    let workspace_ids: BTreeSet<&str> = metadata
        .workspace_members
        .iter()
        .map(String::as_str)
        .collect();
    let packages: BTreeMap<&str, &Package> = metadata
        .packages
        .iter()
        .filter(|package| workspace_ids.contains(package.id.as_str()))
        .map(|package| (package.name.as_str(), package))
        .collect();
    let mut errors = Vec::new();

    check_protected_dependencies(&packages, &mut errors);
    check_owned_dependencies(&packages, &mut errors);
    check_deferred_industrial_dependencies(&packages, &mut errors);
    check_protected_sources(&packages, &mut errors);

    if packages.contains_key("uob-service") {
        documentation::check(root, &mut errors);
        security::check(root, &mut errors);
        check_expected_workspace(&packages, &mut errors);
        check_expected_tests(&packages, &mut errors);
        check_runtime_graphs(&metadata, &packages, &mut errors);
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn load_metadata(root: &Path) -> Result<Metadata, String> {
    let manifest = root.join("Cargo.toml");
    let mut command = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command
        .arg("metadata")
        .arg("--format-version=1")
        .arg("--manifest-path")
        .arg(&manifest);
    if root.join("Cargo.lock").is_file() {
        command.arg("--locked");
    } else {
        command.arg("--no-deps");
    }

    let output = command.output().map_err(|error| {
        format!(
            "could not run Cargo metadata for {}: {error}",
            root.display()
        )
    })?;
    if !output.status.success() {
        return Err(format!(
            "Cargo metadata failed for {}: {}",
            root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("invalid Cargo metadata for {}: {error}", root.display()))
}

fn check_protected_dependencies(packages: &BTreeMap<&str, &Package>, errors: &mut Vec<String>) {
    let policies: [(&str, &[&str]); 3] = [
        (
            "uob-contracts",
            &["jsonschema", "schemars", "serde", "serde_json", "time"],
        ),
        ("uob-domain", &["uob-contracts"]),
        (
            "uob-application",
            &["serde_json", "time", "uob-contracts", "uob-domain"],
        ),
    ];

    for (package_name, allowed) in policies {
        let Some(package) = packages.get(package_name) else {
            continue;
        };
        for dependency in &package.dependencies {
            if !allowed.contains(&dependency.name.as_str()) {
                errors.push(format!(
                    "protected package {package_name} declares forbidden dependency {}",
                    dependency.name
                ));
            }
        }
    }
}

fn check_owned_dependencies(packages: &BTreeMap<&str, &Package>, errors: &mut Vec<String>) {
    for (package_name, package) in packages {
        for dependency in &package.dependencies {
            if dependency.name == "rusqlite" && *package_name != "uob-storage-adapter" {
                errors.push(format!(
                    "{package_name} declares rusqlite, which is owned only by uob-storage-adapter"
                ));
            }
            if dependency.name == "ocpp-client" && *package_name != "uob-sim" {
                errors.push(format!(
                    "{package_name} declares ocpp-client, which is owned only by uob-sim"
                ));
            }
            if dependency.name == "rust-ocpp" && *package_name != "uob-protocol-adapter" {
                errors.push(format!(
                    "{package_name} declares rust-ocpp, which is owned only by uob-protocol-adapter"
                ));
            }
        }
    }
}

fn check_deferred_industrial_dependencies(
    packages: &BTreeMap<&str, &Package>,
    errors: &mut Vec<String>,
) {
    for (package_name, package) in packages {
        for dependency in &package.dependencies {
            if is_opcua_sdk(&dependency.name) {
                errors.push(format!(
                    "{package_name} declares deferred OPC UA dependency {}",
                    dependency.name
                ));
            }
        }
    }
}

fn is_opcua_sdk(name: &str) -> bool {
    let normalized = name.to_ascii_lowercase().replace(['-', '_'], "");
    normalized.contains("opcua") || normalized.contains("open62541")
}

fn check_protected_sources(packages: &BTreeMap<&str, &Package>, errors: &mut Vec<String>) {
    for package_name in ["uob-contracts", "uob-domain", "uob-application"] {
        let Some(package) = packages.get(package_name) else {
            continue;
        };
        let Some(root) = package.manifest_path.parent() else {
            continue;
        };
        scan_rust_sources(package_name, &root.join("src"), errors);
    }
}

fn scan_rust_sources(package_name: &str, directory: &Path, errors: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_rust_sources(package_name, &path, errors);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            let Ok(source) = fs::read_to_string(&path) else {
                errors.push(format!(
                    "could not read protected source {}",
                    path.display()
                ));
                continue;
            };
            let normalized = source.to_ascii_lowercase();
            if let Some(term) = FORBIDDEN_SOURCE_TERMS
                .iter()
                .find(|term| normalized.contains(**term))
            {
                errors.push(format!(
                    "protected package {package_name} contains forbidden source term {term:?} in {}",
                    path.display()
                ));
            }
        }
    }
}

fn check_expected_workspace(packages: &BTreeMap<&str, &Package>, errors: &mut Vec<String>) {
    for expected in EXPECTED_PACKAGES {
        if !packages.contains_key(expected) {
            errors.push(format!("expected workspace package {expected} is missing"));
        }
    }
}

fn check_expected_tests(packages: &BTreeMap<&str, &Package>, errors: &mut Vec<String>) {
    for (package_name, test_name) in [
        ("uob-contracts", "public_schemas"),
        ("uob-release-manager", "release_compatibility"),
        ("uob-storage-adapter", "sqlite_operational_store"),
    ] {
        let Some(package) = packages.get(package_name) else {
            continue;
        };
        let exists = package.targets.iter().any(|target| {
            target.name == test_name && target.kind.iter().any(|kind| kind == "test")
        });
        if !exists {
            errors.push(format!(
                "expected test target {package_name}::{test_name} is missing"
            ));
        }
    }
}

fn check_runtime_graphs(
    metadata: &Metadata,
    packages: &BTreeMap<&str, &Package>,
    errors: &mut Vec<String>,
) {
    let Some(resolve) = &metadata.resolve else {
        errors.push("workspace Cargo metadata did not contain a dependency graph".into());
        return;
    };
    let names_by_id: BTreeMap<&str, &str> = metadata
        .packages
        .iter()
        .map(|package| (package.id.as_str(), package.name.as_str()))
        .collect();
    let edges: BTreeMap<&str, &[String]> = resolve
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node.dependencies.as_slice()))
        .collect();

    check_graph_excludes(
        "uob-service",
        &["uob-sim", "uob-release-manager", "ocpp-client"],
        packages,
        &names_by_id,
        &edges,
        errors,
    );
    check_graph_excludes(
        "uob-sim",
        &[
            "uob-service",
            "uob-application",
            "uob-domain",
            "uob-contracts",
            "uob-protocol-adapter",
            "uob-storage-adapter",
        ],
        packages,
        &names_by_id,
        &edges,
        errors,
    );
    check_graph_excludes(
        "uob-hostile-websocket-peer",
        &[
            "ocpp-client",
            "rust-ocpp",
            "uob-service",
            "uob-protocol-adapter",
            "uob-application",
            "uob-domain",
            "uob-contracts",
        ],
        packages,
        &names_by_id,
        &edges,
        errors,
    );
}

fn check_graph_excludes(
    root_name: &str,
    forbidden: &[&str],
    packages: &BTreeMap<&str, &Package>,
    names_by_id: &BTreeMap<&str, &str>,
    edges: &BTreeMap<&str, &[String]>,
    errors: &mut Vec<String>,
) {
    let Some(root) = packages.get(root_name) else {
        return;
    };
    let mut pending = vec![root.id.as_str()];
    let mut visited = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if !visited.insert(id) {
            continue;
        }
        if id != root.id
            && let Some(name) = names_by_id.get(id)
            && forbidden.contains(name)
        {
            errors.push(format!(
                "{root_name} dependency graph contains forbidden package {name}"
            ));
        }
        if let Some(dependencies) = edges.get(id) {
            pending.extend(dependencies.iter().map(String::as_str));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_opcua_sdk;

    #[test]
    fn deferred_opcua_sdk_names_are_recognized() {
        for name in ["opcua", "async-opcua", "opcua_client", "open62541-sys"] {
            assert!(is_opcua_sdk(name), "{name}");
        }
        assert!(!is_opcua_sdk("uob-target-adapter"));
        assert!(!is_opcua_sdk("rumqttc"));
    }
}
