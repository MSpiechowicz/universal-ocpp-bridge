//! Application-side restrictions complement the packaged network namespace.
use std::{net::IpAddr, path::Path};

use super::{ConfigurationLoadError, FileConfiguration};
use uob_contracts::Environment;

pub(super) fn validate(config: &FileConfiguration) -> Result<(), ConfigurationLoadError> {
    if config.bridge.environment != Environment::Staging {
        return Ok(());
    }
    let fail = ConfigurationLoadError::UnsafeStagingNetwork;
    // Synthetic identifiers and separate ports make accidental production configuration reuse
    // visible even before the host policy is installed. They are not proof of network isolation.
    if !config.bridge.id.starts_with("test-")
        || !config.management.listen_addr.ip().is_loopback()
        || config.management.listen_addr.port() == 8080
        || config.data_export.enabled
    {
        return Err(fail);
    }
    if let Some(endpoint) = &config.events.endpoint {
        let url = reqwest::Url::parse(endpoint).map_err(|_| fail)?;
        if !literal_loopback(&url) || url.port_or_known_default() == Some(8080) {
            return Err(fail);
        }
    }
    if let Some(path) = &config.events.credentials_file {
        staging_file(path)?;
    }
    for target in &config.targets {
        if target.transport.is_some() {
            return Err(fail);
        }
        for (key, value) in &target.settings {
            if key.ends_with("_file") || key.contains("credential") {
                staging_file(value.as_str().ok_or(fail)?)?;
            }
        }
        match target.kind.as_str() {
            "mqtt" => {
                let url = target
                    .settings
                    .get("broker_url")
                    .and_then(toml::Value::as_str)
                    .ok_or(fail)?;
                let url = reqwest::Url::parse(url).map_err(|_| fail)?;
                // A shared/remote broker is intentionally unavailable in the isolated profile.
                // A topic prefix or a self-asserted ACL flag cannot enable one.
                if !literal_loopback(&url)
                    || url.scheme() != "mqtts"
                    || url.port() != Some(18883)
                    || !target.settings.contains_key("credentials_file")
                {
                    return Err(fail);
                }
            }
            "ems-scada.http" => {
                let address = target
                    .settings
                    .get("listen_addr")
                    .and_then(toml::Value::as_str)
                    .and_then(|value| value.parse::<std::net::SocketAddr>().ok())
                    .ok_or(fail)?;
                if !address.ip().is_loopback() || address.port() != 19080 {
                    return Err(fail);
                }
            }
            _ => return Err(fail),
        }
    }
    Ok(())
}

fn literal_loopback(url: &reqwest::Url) -> bool {
    url.host_str().is_some_and(|host| {
        host.trim_matches(['[', ']'])
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
    })
}

fn staging_file(value: &str) -> Result<(), ConfigurationLoadError> {
    let path = Path::new(value);
    if !path.starts_with("/etc/uob-staging")
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(ConfigurationLoadError::UnsafeStagingNetwork);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(document: &str) -> Result<(), ConfigurationLoadError> {
        validate(&toml::from_str(document).unwrap())
    }

    const BASE: &str = "[bridge]\nid='test-site-01'\nenvironment='staging'\n[management]\nlisten_addr='127.0.0.1:18080'\n";
    const MQTT: &str = "[[targets]]\nid='test-main'\nkind='mqtt'\nenabled=true\n[targets.settings]\nbroker_url='mqtts://127.0.0.1:18883'\ncredentials_file='/etc/uob-staging/mqtt.toml'\n";

    #[test]
    fn isolated_test_peers_are_valid_and_production_is_unchanged() {
        assert!(check(BASE).is_ok());
        assert!(check(&format!("{BASE}{MQTT}")).is_ok());
        assert!(check("[bridge]\nid='site-01'\nenvironment='production'\n").is_ok());
        assert!(check(&format!("{BASE}[[targets]]\nid='test-main'\nkind='ems-scada.http'\n[targets.settings]\nlisten_addr='127.0.0.1:19080'\n")).is_ok());
    }

    #[test]
    fn production_endpoints_credentials_and_topic_only_isolation_are_rejected() {
        for document in [
            BASE.replace("test-site-01", "site-01"),
            BASE.replace("18080", "8080"),
            BASE.replace("127.0.0.1", "0.0.0.0"),
            format!("{BASE}[events]\nendpoint='https://production.example/events'\n"),
            format!("{BASE}[events]\nendpoint='http://127.0.0.1:8080/api/v1/events'\n"),
            format!("{BASE}{}", MQTT.replace("127.0.0.1", "production.example")),
            format!("{BASE}{}", MQTT.replace("18883", "8883")),
            format!("{BASE}{}", MQTT.replace("127.0.0.1", "localhost")),
            format!("{BASE}{}", MQTT.replace("/etc/uob-staging/", "/etc/uob/")),
            format!(
                "{BASE}{}",
                MQTT.replace("/etc/uob-staging/", "/etc/uob-staging/../uob/")
            ),
            format!(
                "{BASE}{}",
                MQTT.replace("credentials_file='/etc/uob-staging/mqtt.toml'", "")
            ),
            format!("{BASE}[[targets]]\nid='test'\nkind='production-payment'\n"),
        ] {
            assert_eq!(
                check(&document),
                Err(ConfigurationLoadError::UnsafeStagingNetwork),
                "{document}"
            );
        }
    }
}
