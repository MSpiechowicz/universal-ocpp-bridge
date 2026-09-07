//! Signed, bounded application bundle installation. Activation is a separate gate.
pub(crate) mod filesystem;
pub(crate) mod manifest;
mod store;

pub use manifest::{BundleFile, BundleManifest, InstallPolicy, verify_manifest};
pub use store::{ArtifactStore, InstalledArtifact};

use std::{error::Error, fmt, io};

/// Fail-closed installer failure. No error authorizes activation.
#[derive(Debug)]
pub enum InstallError {
    Io(io::Error),
    Json(serde_json::Error),
    Rejected(&'static str),
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "artifact I/O: {error}"),
            Self::Json(error) => write!(f, "artifact metadata: {error}"),
            Self::Rejected(reason) => write!(f, "artifact rejected: {reason}"),
        }
    }
}
impl Error for InstallError {}
impl From<io::Error> for InstallError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<serde_json::Error> for InstallError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}
impl From<rustix::io::Errno> for InstallError {
    fn from(error: rustix::io::Errno) -> Self {
        Self::Io(error.into())
    }
}

fn reject<T>(reason: &'static str) -> Result<T, InstallError> {
    Err(InstallError::Rejected(reason))
}
