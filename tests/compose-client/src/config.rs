use serde::Deserialize;
use std::{collections::HashSet, path::Path, time::Duration};

use crate::{Error, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Http,
    Mqtt,
    EmsMqtt,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub bridge_id: String,
    pub environment: String,
    pub target_instance_id: String,
    pub station: Vec<Station>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Station {
    pub id: String,
    pub protocol: String,
    pub authorization_reference: String,
}

pub struct Args {
    pub mode: Mode,
    pub config: Config,
    pub http_base: Option<String>,
    pub read_token: Option<String>,
    pub control_token: Option<String>,
    pub broker_url: Option<String>,
    pub mqtt_username: Option<String>,
    pub mqtt_password: Option<String>,
    pub ca: Option<Vec<u8>>,
    pub timeout: Duration,
}

fn file(path: &str, limit: u64) -> Result<Vec<u8>> {
    let size = std::fs::metadata(path)
        .map_err(|_| Error("credential/config file unavailable"))?
        .len();
    if size == 0 || size > limit {
        return Err(Error("credential/config file exceeds bound or is empty"));
    }
    std::fs::read(path).map_err(|_| Error("credential/config file unreadable"))
}

fn secret(path: &str) -> Result<String> {
    let bytes = file(path, 64 * 1024)?;
    let text = String::from_utf8(bytes).map_err(|_| Error("credential is not UTF-8"))?;
    let value = text.trim_end_matches(['\r', '\n']);
    if value.is_empty() || value.contains(['\r', '\n']) {
        return Err(Error("credential is empty or multiline"));
    }
    Ok(value.to_owned())
}

fn option<'a>(items: &'a std::collections::BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    items.get(key).map(String::as_str)
}

fn required<'a>(
    items: &'a std::collections::BTreeMap<String, String>,
    key: &str,
) -> Result<&'a str> {
    option(items, key).ok_or(Error("missing required CLI option"))
}

impl Args {
    pub fn parse() -> Result<Self> {
        let supplied = Self::options()?;
        let mode = match required(&supplied, "--mode")? {
            "http" => Mode::Http,
            "mqtt" => Mode::Mqtt,
            "ems-mqtt" => Mode::EmsMqtt,
            _ => return Err(Error("unsupported mode")),
        };
        let config = Self::config(Path::new(required(&supplied, "--config")?))?;
        let seconds = option(&supplied, "--timeout-seconds")
            .unwrap_or("120")
            .parse::<u64>()
            .map_err(|_| Error("invalid timeout"))?;
        if !(5..=300).contains(&seconds) {
            return Err(Error("timeout must be between 5 and 300 seconds"));
        }
        let result = Self {
            mode,
            config,
            http_base: option(&supplied, "--http-base").map(str::to_owned),
            read_token: option(&supplied, "--read-token-file")
                .map(secret)
                .transpose()?,
            control_token: option(&supplied, "--control-token-file")
                .map(secret)
                .transpose()?,
            broker_url: option(&supplied, "--broker-url").map(str::to_owned),
            mqtt_username: option(&supplied, "--mqtt-username").map(str::to_owned),
            mqtt_password: option(&supplied, "--mqtt-password-file")
                .map(secret)
                .transpose()?,
            ca: option(&supplied, "--ca-file")
                .map(|path| file(path, 1024 * 1024))
                .transpose()?,
            timeout: Duration::from_secs(seconds),
        };
        match mode {
            Mode::Http => {
                if result.http_base.is_none()
                    || result.read_token.is_none()
                    || result.control_token.is_none()
                    || result.broker_url.is_some()
                    || result.mqtt_username.is_some()
                    || result.mqtt_password.is_some()
                    || result.ca.is_some()
                {
                    return Err(Error(
                        "HTTP mode requires only HTTP endpoint and both token files",
                    ));
                }
            }
            Mode::Mqtt | Mode::EmsMqtt => {
                if result.broker_url.is_none()
                    || result.mqtt_username.as_deref().is_none_or(str::is_empty)
                    || result.mqtt_password.is_none()
                    || result.http_base.is_some()
                    || result.read_token.is_some()
                    || result.control_token.is_some()
                {
                    return Err(Error(
                        "MQTT mode requires only broker endpoint, username and password file",
                    ));
                }
                let broker = url::Url::parse(result.broker_url.as_deref().unwrap_or_default())
                    .map_err(|_| Error("invalid broker URL"))?;
                if !matches!(broker.scheme(), "mqtt" | "mqtts")
                    || !broker.username().is_empty()
                    || broker.password().is_some()
                    || broker.query().is_some()
                    || broker.fragment().is_some()
                    || !matches!(broker.path(), "" | "/")
                    || (broker.scheme() == "mqtts") != result.ca.is_some()
                    || (broker.scheme() == "mqtt"
                        && !matches!(broker.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")))
                {
                    return Err(Error(
                        "broker URL must be credential-free; TLS requires CA file",
                    ));
                }
            }
        }
        result.config.validate()?;
        Ok(result)
    }

    fn options() -> Result<std::collections::BTreeMap<String, String>> {
        let mut supplied = std::collections::BTreeMap::new();
        let mut args = std::env::args().skip(1);
        while let Some(key) = args.next() {
            if !matches!(
                key.as_str(),
                "--mode"
                    | "--config"
                    | "--http-base"
                    | "--read-token-file"
                    | "--control-token-file"
                    | "--broker-url"
                    | "--mqtt-username"
                    | "--mqtt-password-file"
                    | "--ca-file"
                    | "--timeout-seconds"
            ) {
                return Err(Error("unknown CLI option"));
            }
            let value = args.next().ok_or(Error("CLI option has no value"))?;
            if supplied.insert(key, value).is_some() {
                return Err(Error("duplicate CLI option"));
            }
        }
        Ok(supplied)
    }

    fn config(path: &Path) -> Result<Config> {
        let bytes = file(
            path.to_str().ok_or(Error("invalid config path"))?,
            64 * 1024,
        )?;
        toml::from_str(std::str::from_utf8(&bytes).map_err(|_| Error("invalid config UTF-8"))?)
            .map_err(|_| Error("invalid client TOML"))
    }
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        if self.bridge_id.is_empty()
            || self.target_instance_id.is_empty()
            || self.environment != "demo"
            || self.station.len() != 2
            || self
                .station
                .iter()
                .any(|s| s.id.is_empty() || s.authorization_reference.is_empty())
            || !self.station.iter().any(|s| s.protocol == "ocpp16")
            || !self.station.iter().any(|s| s.protocol == "ocpp201")
            || self
                .station
                .iter()
                .any(|s| !matches!(s.protocol.as_str(), "ocpp16" | "ocpp201"))
            || self
                .station
                .iter()
                .map(|s| &s.id)
                .collect::<HashSet<_>>()
                .len()
                != 2
        {
            return Err(Error(
                "config must define distinct OCPP 1.6 and 2.0.1 demo stations",
            ));
        }
        Ok(())
    }
}
