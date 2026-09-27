use std::{
    fs::{File, OpenOptions},
    io::Read as _,
    path::Path,
};

use serde::Deserialize;
use uob_application::DatabaseError;

use crate::{
    errors::invalid,
    settings::{Settings, credential_path},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialFile {
    username: String,
    password: String,
    ca_certificate_file: Option<String>,
}

pub(crate) struct Credentials {
    pub username: String,
    pub password: String,
    pub ca: Option<Vec<u8>>,
}

pub(crate) fn resolve(settings: &Settings) -> Result<Credentials, DatabaseError> {
    let bytes = read_file(&credential_path(settings), 64 * 1024, true)
        .map_err(|()| invalid("postgres.credentials.unavailable"))?;
    let document: CredentialFile =
        toml::from_slice(&bytes).map_err(|_| invalid("postgres.credentials.invalid"))?;
    if document.username.is_empty()
        || document.password.is_empty()
        || document.username.len() > 63
        || document.password.len() > 4096
        || document.username.bytes().any(|byte| byte == 0)
        || document.password.bytes().any(|byte| byte == 0)
    {
        return Err(invalid("postgres.credentials.invalid"));
    }
    let ca = match document.ca_certificate_file {
        Some(path) if settings.tls => Some(
            read_file(Path::new(&path), 1024 * 1024, false)
                .map_err(|()| invalid("postgres.ca.unavailable"))?,
        ),
        None if !settings.tls => None,
        _ => return Err(invalid("postgres.ca.invalid")),
    };
    Ok(Credentials {
        username: document.username,
        password: document.password,
        ca,
    })
}

fn read_file(path: &Path, maximum: u64, private: bool) -> Result<Vec<u8>, ()> {
    #[cfg(not(unix))]
    if std::fs::symlink_metadata(path)
        .map_err(|_| ())?
        .file_type()
        .is_symlink()
    {
        return Err(());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|_| ())?;
    validate_handle(&file, maximum, private)?;
    let mut bytes = Vec::new();
    file.take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() as u64 > maximum {
        return Err(());
    }
    Ok(bytes)
}

fn validate_handle(file: &File, maximum: u64, private: bool) -> Result<(), ()> {
    let metadata = file.metadata().map_err(|_| ())?;
    if !metadata.is_file() || metadata.len() > maximum {
        return Err(());
    }
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(());
        }
    }
    #[cfg(not(unix))]
    let _ = private;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::read_file;
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };

    #[test]
    fn private_files_reject_world_access_symlinks_and_directories() {
        let path = std::env::temp_dir().join(format!("uob-pg-secret-{}", std::process::id()));
        fs::write(&path, b"secret").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_file(&path, 6, true).unwrap(), b"secret");
        assert!(read_file(&path, 5, true).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_file(&path, 6, true).is_err());
        let link = path.with_extension("link");
        symlink(&path, &link).unwrap();
        assert!(read_file(&link, 6, true).is_err());
        fs::remove_file(link).unwrap();
        fs::remove_file(path).unwrap();
    }
}
