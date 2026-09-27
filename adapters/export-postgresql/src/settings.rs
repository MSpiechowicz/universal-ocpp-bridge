use std::{net::SocketAddr, path::PathBuf};

use uob_application::{CredentialReference, DatabaseError};
use uob_contracts::Environment;
use uob_external_export_adapter::{
    DatabaseTransportSecurity, validate_database_transport_security,
};

use crate::errors::invalid;

pub(crate) struct Settings {
    pub endpoint: SocketAddr,
    pub hostname: String,
    pub database: String,
    pub credential: CredentialReference,
    pub tls: bool,
}

impl Settings {
    pub fn parse(args: &[String]) -> Result<(Self, &str, Option<&str>), DatabaseError> {
        if !(args.len() == 7 || args.len() == 8) {
            return Err(invalid("postgres.settings.invalid"));
        }
        let environment = match args[1].as_str() {
            "production" => Environment::Production,
            "staging" => Environment::Staging,
            "demo-isolated" => Environment::Demo,
            _ => return Err(invalid("postgres.settings.invalid")),
        };
        let tls = match args[2].as_str() {
            "verified" => true,
            "plaintext" => false,
            _ => return Err(invalid("postgres.settings.invalid")),
        };
        let endpoint: SocketAddr = args[3]
            .parse()
            .map_err(|_| invalid("postgres.endpoint.invalid"))?;
        // The demo label alone cannot establish isolation. Reject before opening either
        // the credential file or a socket, including IPv4-mapped/non-loopback IPv6.
        if !tls && (environment != Environment::Demo || !endpoint.ip().is_loopback()) {
            return Err(invalid("postgres.transport.invalid"));
        }
        let hostname = &args[4];
        let database = &args[5];
        if !bounded_identifier(hostname) || !bounded_identifier(database) {
            return Err(invalid("postgres.settings.invalid"));
        }
        let credential = CredentialReference::new(args[6].clone())
            .map_err(|_| invalid("postgres.credentials.invalid"))?;
        let security = DatabaseTransportSecurity {
            tls,
            certificate_verification: tls,
            credentials_file: Some(credential.clone()),
            explicitly_isolated: environment == Environment::Demo && endpoint.ip().is_loopback(),
        };
        validate_database_transport_security(environment, &security)
            .map_err(|_| invalid("postgres.transport.invalid"))?;
        let scenario = match args[0].as_str() {
            "probe" | "sleep" | "transaction" | "cancel" | "stress" | "stress-recovery"
            | "stress-cancel" | "stress-rollback" => args[0].as_str(),
            _ => return Err(invalid("postgres.scenario.invalid")),
        };
        let marker = args.get(7).map(String::as_str);
        if marker.is_some_and(|value| !bounded_identifier(value))
            || ((scenario == "transaction") != marker.is_some())
        {
            return Err(invalid("postgres.marker.invalid"));
        }
        Ok((
            Self {
                endpoint,
                hostname: hostname.clone(),
                database: database.clone(),
                credential,
                tls,
            },
            scenario,
            marker,
        ))
    }
}

fn bounded_identifier(value: &str) -> bool {
    (1..=63).contains(&value.len())
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-' || byte == b'.'
        })
}

pub(crate) fn credential_path(settings: &Settings) -> PathBuf {
    PathBuf::from(settings.credential.as_str())
}
