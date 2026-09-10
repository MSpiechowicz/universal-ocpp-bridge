use serde::Deserialize;
use std::{error::Error, path::Path};
use uob_release_manager::{
    artifacts::InstallError,
    supervisor::{Grant, Supervisor, ipc::Server},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    state_directory: std::path::PathBuf,
    runtime_directory: std::path::PathBuf,
    artifact_store: std::path::PathBuf,
    install_policy: std::path::PathBuf,
    grants: Vec<Grant>,
    qualification_policy: Option<std::path::PathBuf>,
    preflight_policy: Option<std::path::PathBuf>,
}

pub fn run(path: &Path) -> Result<(), Box<dyn Error>> {
    let config: Configuration =
        serde_json::from_slice(&crate::artifact_cli::read(path, 64 * 1024, true)?)?;
    // Never accept client-supplied trust keys or host claims. Use the installer's
    // existing actual-host check before exposing local release controls.
    let policy = crate::artifact_cli::policy(&config.install_policy)?;
    if config
        .runtime_directory
        .starts_with(&config.state_directory)
        || config
            .state_directory
            .starts_with(&config.runtime_directory)
        || config.runtime_directory.starts_with(&config.artifact_store)
        || config.artifact_store.starts_with(&config.runtime_directory)
    {
        return Err(InstallError::Rejected("supervisor directories must be separate").into());
    }
    let mut supervisor = Supervisor::open(
        &config.state_directory,
        &config.artifact_store,
        policy,
        config.grants,
    )?;
    if let Some(path) = config.qualification_policy {
        let policy = serde_json::from_slice(&crate::artifact_cli::read(&path, 64 * 1024, true)?)?;
        supervisor = supervisor.with_qualification_policy(policy)?;
    }
    if let Some(path) = config.preflight_policy {
        let policy = serde_json::from_slice(&crate::artifact_cli::read(&path, 64 * 1024, true)?)?;
        supervisor = supervisor.with_preflight_policy(policy)?;
    }
    let server = Server::bind(&config.runtime_directory)?;
    server.run(supervisor)?;
    Ok(())
}
