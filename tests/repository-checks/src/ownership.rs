use std::collections::BTreeMap;

use super::{Dependency, Package};

pub(super) fn check_owned_dependencies(
    packages: &BTreeMap<&str, &Package>,
    errors: &mut Vec<String>,
) {
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
            if is_rumqtt_dependency(dependency)
                && *package_name != "uob-mqtt-target-adapter"
                && !isolated_compose_client(package_name, package)
            {
                errors.push(format!(
                    "{package_name} declares {}, which is owned only by uob-mqtt-target-adapter or the isolated Compose verification client",
                    dependency.name
                ));
            }
        }
    }
}

pub(super) fn is_rumqtt_dependency(dependency: &Dependency) -> bool {
    matches!(dependency.name.as_str(), "rumqttc" | "rumqttc-v4-next")
        || dependency.rename.as_deref() == Some("rumqttc")
}

// The external EMS test peer must speak MQTT independently of the target adapter.
// Restrict that exception to its binary-only integration package, not application code.
fn isolated_compose_client(name: &str, package: &Package) -> bool {
    name == "uob-compose-client"
        && package
            .manifest_path
            .ends_with("tests/compose-client/Cargo.toml")
        && !package.targets.is_empty()
        && package
            .targets
            .iter()
            .all(|target| target.kind.iter().any(|kind| kind == "bin"))
}
#[cfg(test)]
mod tests {
    use super::isolated_compose_client;
    use crate::{Package, Target};
    use std::path::PathBuf;

    #[test]
    fn only_binary_compose_peer_gets_test_mqtt_exception() {
        let mut package = Package {
            id: "compose".into(),
            name: "uob-compose-client".into(),
            manifest_path: PathBuf::from("/repo/tests/compose-client/Cargo.toml"),
            dependencies: vec![],
            targets: vec![Target {
                name: "uob-compose-client".into(),
                kind: vec!["bin".into()],
            }],
        };
        assert!(isolated_compose_client("uob-compose-client", &package));
        assert!(!isolated_compose_client("uob-service", &package));
        package.manifest_path = PathBuf::from("/repo/bins/uob-service/Cargo.toml");
        assert!(!isolated_compose_client("uob-compose-client", &package));
        package.manifest_path = PathBuf::from("/repo/tests/compose-client/Cargo.toml");
        package.targets[0].kind = vec!["lib".into()];
        assert!(!isolated_compose_client("uob-compose-client", &package));
    }
}
