use std::{io::Write, path::PathBuf};

#[path = "cli_serve.rs"]
mod serving;

use crate::{configuration, event_stream, release_cli};

const DEFAULT_CONFIGURATION_PATH: &str = "bridge.toml";

enum Command {
    Serve {
        configuration: PathBuf,
        no_ui: bool,
    },
    Check {
        configuration: PathBuf,
        secrets: bool,
    },
    Events {
        configuration: PathBuf,
        after: Option<String>,
    },
    Release(release_cli::Command),
}

pub struct CliResult {
    pub exit_code: u8,
    pub diagnostic: Option<String>,
}

pub async fn execute(
    arguments: impl IntoIterator<Item = String>,
    output: &mut impl Write,
) -> CliResult {
    let mut arguments = arguments.into_iter().peekable();
    let release = arguments
        .peek()
        .is_some_and(|argument| argument == "release");
    let Ok(command) = parse_arguments(arguments) else {
        if release {
            let _ = serde_json::to_writer(
                &mut *output,
                &serde_json::json!({ "error": "invalid_release_arguments" }),
            )
            .and_then(|()| output.write_all(b"\n").map_err(serde_json::Error::io));
        }
        return failure(2, usage());
    };
    match command {
        Command::Serve {
            configuration,
            no_ui,
        } => serving::serve(&configuration, no_ui).await,
        Command::Check {
            configuration,
            secrets,
        } => check(&configuration, secrets, output),
        Command::Events {
            configuration,
            after,
        } => events(&configuration, after.as_deref(), output).await,
        Command::Release(command) => match release_cli::execute(command, output).await {
            Ok(()) => success(),
            Err(error) => {
                let diagnostic = error.diagnostic().to_owned();
                error.write_safe(output);
                failure(1, diagnostic)
            }
        },
    }
}

fn check(
    configuration_path: &std::path::Path,
    secrets: bool,
    output: &mut impl Write,
) -> CliResult {
    if let Err(error) = configuration::load(configuration_path) {
        return failure(2, error.to_string());
    }
    if secrets && let Err(error) = configuration::check_secrets(configuration_path) {
        return failure(2, error.to_string());
    }
    if serde_json::to_writer(&mut *output, &serde_json::json!({ "status": "valid" }))
        .and_then(|()| output.write_all(b"\n").map_err(serde_json::Error::io))
        .is_err()
    {
        return failure(1, "command output unavailable".to_owned());
    }
    success()
}

async fn events(
    configuration_path: &std::path::Path,
    after: Option<&str>,
    output: &mut impl Write,
) -> CliResult {
    let configuration = match configuration::load(configuration_path) {
        Ok(configuration) => configuration,
        Err(error) => return failure(2, error.to_string()),
    };
    if let Err(error) = crate::staging_network::verify(
        configuration
            .service
            .application
            .identity()
            .runtime
            .environment,
    ) {
        return failure(1, error.to_owned());
    }
    match event_stream::stream(&configuration.events, after, output).await {
        Ok(()) => success(),
        Err(error) => failure(1, error.to_string()),
    }
}

fn parse_arguments(arguments: impl IntoIterator<Item = String>) -> Result<Command, ()> {
    let mut arguments = arguments.into_iter();
    match arguments.next().as_deref() {
        Some("serve") => parse_serve(arguments),
        Some("config") if arguments.next().as_deref() == Some("check") => parse_check(arguments),
        Some("events") => parse_events(arguments),
        Some("release") => parse_release(arguments),
        _ => Err(()),
    }
}

fn parse_serve(mut arguments: impl Iterator<Item = String>) -> Result<Command, ()> {
    let mut configuration = None;
    let mut no_ui = false;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--config" if configuration.is_none() => {
                configuration = arguments.next().map(Into::into);
            }
            "--no-ui" if !no_ui => no_ui = true,
            _ => return Err(()),
        }
    }
    Ok(Command::Serve {
        configuration: configuration.ok_or(())?,
        no_ui,
    })
}

fn parse_check(mut arguments: impl Iterator<Item = String>) -> Result<Command, ()> {
    let configuration = match (arguments.next().as_deref(), arguments.next()) {
        (Some("--config"), Some(path)) => PathBuf::from(path),
        _ => return Err(()),
    };
    let secrets = match arguments.next().as_deref() {
        None => false,
        Some("--secrets") => true,
        _ => return Err(()),
    };
    if arguments.next().is_some() {
        return Err(());
    }
    Ok(Command::Check {
        configuration,
        secrets,
    })
}

fn parse_events(mut arguments: impl Iterator<Item = String>) -> Result<Command, ()> {
    let mut configuration = PathBuf::from(DEFAULT_CONFIGURATION_PATH);
    let mut configuration_seen = false;
    let mut format_seen = false;
    let mut after = None;
    while let Some(argument) = arguments.next() {
        let value = arguments.next().ok_or(())?;
        match argument.as_str() {
            "--config" if !configuration_seen => {
                configuration = value.into();
                configuration_seen = true;
            }
            "--format" if !format_seen && value == "jsonl" => format_seen = true,
            "--after" if after.is_none() && !value.trim().is_empty() => after = Some(value),
            _ => return Err(()),
        }
    }
    if !format_seen {
        return Err(());
    }
    Ok(Command::Events {
        configuration,
        after,
    })
}

fn parse_release(mut arguments: impl Iterator<Item = String>) -> Result<Command, ()> {
    let operation = arguments.next().ok_or(())?;
    let command = match operation.as_str() {
        "stage" => parse_release_stage(arguments)?,
        "qualify" => parse_release_qualify(arguments)?,
        "promote" => parse_release_promote(arguments)?,
        "rollback" => parse_release_rollback(arguments)?,
        "status" => parse_release_status(arguments)?,
        "events" => parse_release_events(arguments)?,
        _ => return Err(()),
    };
    Ok(Command::Release(command))
}

fn parse_release_stage(
    mut arguments: impl Iterator<Item = String>,
) -> Result<release_cli::Command, ()> {
    let mut bundle = None;
    let mut socket = PathBuf::from(release_cli::DEFAULT_SOCKET);
    let mut socket_seen = false;
    while let Some(option) = arguments.next() {
        let value = arguments.next().ok_or(())?;
        match option.as_str() {
            "--bundle" if bundle.is_none() => bundle = Some(value.into()),
            "--socket" if !socket_seen => {
                socket = value.into();
                socket_seen = true;
            }
            _ => return Err(()),
        }
    }
    Ok(release_cli::Command::Stage {
        bundle: bundle.ok_or(())?,
        socket,
    })
}

fn parse_release_qualify(
    mut arguments: impl Iterator<Item = String>,
) -> Result<release_cli::Command, ()> {
    let mut release = None;
    let mut evidence = None;
    let mut socket = PathBuf::from(release_cli::DEFAULT_SOCKET);
    let mut socket_seen = false;
    while let Some(option) = arguments.next() {
        let value = arguments.next().ok_or(())?;
        match option.as_str() {
            "--release" if release.is_none() && release_cli::digest_name(&value) => {
                release = Some(value);
            }
            "--evidence" if evidence.is_none() => evidence = Some(value.into()),
            "--socket" if !socket_seen => {
                socket = value.into();
                socket_seen = true;
            }
            _ => return Err(()),
        }
    }
    Ok(release_cli::Command::Qualify {
        release: release.ok_or(())?,
        evidence: evidence.ok_or(())?,
        socket,
    })
}

fn parse_release_promote(
    mut arguments: impl Iterator<Item = String>,
) -> Result<release_cli::Command, ()> {
    let mut release = None;
    let mut socket = PathBuf::from(release_cli::DEFAULT_SOCKET);
    let mut socket_seen = false;
    while let Some(option) = arguments.next() {
        let value = arguments.next().ok_or(())?;
        match option.as_str() {
            "--release" if release.is_none() && release_cli::digest_name(&value) => {
                release = Some(value);
            }
            "--socket" if !socket_seen => {
                socket = value.into();
                socket_seen = true;
            }
            _ => return Err(()),
        }
    }
    Ok(release_cli::Command::Promote {
        release: release.ok_or(())?,
        socket,
    })
}

fn parse_release_rollback(
    mut arguments: impl Iterator<Item = String>,
) -> Result<release_cli::Command, ()> {
    let mut previous_good = false;
    let mut socket = PathBuf::from(release_cli::DEFAULT_SOCKET);
    let mut socket_seen = false;
    while let Some(option) = arguments.next() {
        let value = arguments.next().ok_or(())?;
        match option.as_str() {
            "--to" if !previous_good && value == "previous-good" => previous_good = true,
            "--socket" if !socket_seen => {
                socket = value.into();
                socket_seen = true;
            }
            _ => return Err(()),
        }
    }
    previous_good
        .then_some(release_cli::Command::Rollback { socket })
        .ok_or(())
}

fn parse_release_status(
    mut arguments: impl Iterator<Item = String>,
) -> Result<release_cli::Command, ()> {
    let mut json = false;
    let mut socket = PathBuf::from(release_cli::DEFAULT_SOCKET);
    let mut socket_seen = false;
    while let Some(option) = arguments.next() {
        let value = arguments.next().ok_or(())?;
        match option.as_str() {
            "--format" if !json && value == "json" => json = true,
            "--socket" if !socket_seen => {
                socket = value.into();
                socket_seen = true;
            }
            _ => return Err(()),
        }
    }
    json.then_some(release_cli::Command::Status { socket })
        .ok_or(())
}

fn parse_release_events(
    mut arguments: impl Iterator<Item = String>,
) -> Result<release_cli::Command, ()> {
    let mut jsonl = false;
    let mut after = 0;
    let mut after_seen = false;
    let mut socket = PathBuf::from(release_cli::DEFAULT_SOCKET);
    let mut socket_seen = false;
    while let Some(option) = arguments.next() {
        let value = arguments.next().ok_or(())?;
        match option.as_str() {
            "--format" if !jsonl && value == "jsonl" => jsonl = true,
            "--after" if !after_seen => {
                after = value.parse().map_err(|_| ())?;
                after_seen = true;
            }
            "--socket" if !socket_seen => {
                socket = value.into();
                socket_seen = true;
            }
            _ => return Err(()),
        }
    }
    jsonl
        .then_some(release_cli::Command::Events { after, socket })
        .ok_or(())
}

fn success() -> CliResult {
    CliResult {
        exit_code: 0,
        diagnostic: None,
    }
}

fn failure(exit_code: u8, diagnostic: String) -> CliResult {
    CliResult {
        exit_code,
        diagnostic: Some(diagnostic),
    }
}

fn usage() -> String {
    "usage: uob serve --config PATH [--no-ui] | uob config check --config PATH [--secrets] | uob events [--config PATH] [--after CURSOR] --format jsonl | uob release {stage --bundle DIRECTORY | qualify --release DIGEST --evidence FILE | promote --release DIGEST | rollback --to previous-good | status --format json | events --format jsonl [--after SEQUENCE]} [--socket PATH]".to_owned()
}

#[cfg(test)]
mod tests {
    use super::{Command, parse_arguments};

    #[test]
    fn planned_commands_are_strict_and_noninteractive() {
        assert!(matches!(
            parse_arguments(["serve", "--config", "bridge.toml", "--no-ui"].map(str::to_owned)),
            Ok(Command::Serve { no_ui: true, .. })
        ));
        assert!(matches!(
            parse_arguments(["config", "check", "--config", "bridge.toml"].map(str::to_owned)),
            Ok(Command::Check { .. })
        ));
        assert!(matches!(
            parse_arguments(["events", "--format", "jsonl"].map(str::to_owned)),
            Ok(Command::Events { .. })
        ));
        assert!(parse_arguments(["serve"].map(str::to_owned)).is_err());
        assert!(parse_arguments(["events", "--format", "text"].map(str::to_owned)).is_err());
        let digest = "a".repeat(64);
        assert!(matches!(
            parse_arguments(
                [
                    "release",
                    "status",
                    "--format",
                    "json",
                    "--socket",
                    "/tmp/control.sock"
                ]
                .map(str::to_owned)
            ),
            Ok(Command::Release(_))
        ));
        assert!(
            parse_arguments(
                [
                    "release", "events", "--format", "jsonl", "--after", "1", "--after", "2"
                ]
                .map(str::to_owned)
            )
            .is_err()
        );
        assert!(
            parse_arguments(
                [
                    "release",
                    "promote",
                    "--release",
                    &digest,
                    "--release",
                    &digest
                ]
                .map(str::to_owned)
            )
            .is_err()
        );
    }
}
