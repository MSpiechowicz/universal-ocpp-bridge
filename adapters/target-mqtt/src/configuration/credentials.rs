use std::path::PathBuf;

use serde::Deserialize;
use uob_application::CredentialReference;

use self::bounded_file::read_bounded_file;

mod bounded_file;

const MAX_CREDENTIAL_FILE_BYTES: u64 = 64 * 1024;
const MAX_CERTIFICATE_FILE_BYTES: u64 = 1024 * 1024;

pub(crate) struct ResolvedCredentials {
    pub(crate) login: Option<(String, String)>,
    pub(crate) certificate_authority: Option<Vec<u8>>,
    pub(crate) client_authentication: Option<(Vec<u8>, Vec<u8>)>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialFile {
    username: Option<String>,
    password: Option<String>,
    ca_certificate_file: Option<PathBuf>,
    client_certificate_file: Option<PathBuf>,
    client_private_key_file: Option<PathBuf>,
}

pub(crate) fn resolve_credentials(
    reference: Option<&CredentialReference>,
) -> Result<Option<ResolvedCredentials>, &'static str> {
    let Some(reference) = reference else {
        return Ok(None);
    };
    let path = PathBuf::from(reference.as_str());
    let source = read_bounded_file(&path, MAX_CREDENTIAL_FILE_BYTES, true)
        .map_err(|()| "mqtt.credentials_unavailable")?;
    let document: CredentialFile =
        toml::from_slice(&source).map_err(|_| "mqtt.credentials_invalid")?;
    let login = match (document.username, document.password) {
        (Some(username), Some(password)) if !username.is_empty() && !password.is_empty() => {
            Some((username, password))
        }
        (None, None) => None,
        _ => return Err("mqtt.credentials_invalid"),
    };
    let certificate_authority = document
        .ca_certificate_file
        .map(|path| read_bounded_file(&path, MAX_CERTIFICATE_FILE_BYTES, false))
        .transpose()
        .map_err(|()| "mqtt.tls_material_unavailable")?;
    let client_authentication = match (
        document.client_certificate_file,
        document.client_private_key_file,
    ) {
        (Some(certificate), Some(private_key)) if certificate_authority.is_some() => Some((
            read_bounded_file(&certificate, MAX_CERTIFICATE_FILE_BYTES, false)
                .map_err(|()| "mqtt.tls_material_unavailable")?,
            read_bounded_file(&private_key, MAX_CERTIFICATE_FILE_BYTES, true)
                .map_err(|()| "mqtt.tls_material_unavailable")?,
        )),
        (None, None) => None,
        _ => return Err("mqtt.credentials_invalid"),
    };
    if login.is_none() && client_authentication.is_none() {
        return Err("mqtt.credentials_invalid");
    }
    Ok(Some(ResolvedCredentials {
        login,
        certificate_authority,
        client_authentication,
    }))
}
