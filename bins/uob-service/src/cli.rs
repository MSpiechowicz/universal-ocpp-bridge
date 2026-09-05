use std::{io::Write, path::PathBuf};

use crate::{configuration, event_stream};

const DEFAULT_CONFIGURATION_PATH: &str = "bridge.toml";

enum Command {
    Serve {
        configuration: PathBuf,
        no_ui: bool,
    },
    Check {
        configuration: PathBuf,
    },
    Events {
        configuration: PathBuf,
        after: Option<String>,
    },
}

pub struct CliResult {
    pub exit_code: u8,
    pub diagnostic: Option<String>,
}

pub async fn execute(
    arguments: impl IntoIterator<Item = String>,
    output: &mut impl Write,
) -> CliResult {
    let Ok(command) = parse_arguments(arguments) else {
        return failure(2, usage());
    };
    match command {
        Command::Serve {
            configuration,
            no_ui,
        } => serve(&configuration, no_ui).await,
        Command::Check { configuration } => check(&configuration, output),
        Command::Events {
            configuration,
            after,
        } => events(&configuration, after.as_deref(), output).await,
    }
}

async fn serve(configuration_path: &std::path::Path, no_ui: bool) -> CliResult {
    let configuration = match configuration::load(configuration_path) {
        Ok(configuration) => configuration,
        Err(error) => return failure(2, error.to_string()),
    };
    let deployment = match configuration
        .deployment
        .as_ref()
        .map(|layout| layout.open(configuration.service.application.identity()))
        .transpose()
    {
        Ok(deployment) => deployment,
        Err(error) => return failure(1, error.to_owned()),
    };
    let options = uob_management_adapter::ManagementRouterOptions {
        static_assets: !no_ui,
    };
    eprintln!(
        "service listening on {} (static assets: {})",
        configuration.management_address,
        if no_ui { "disabled" } else { "enabled" }
    );
    let result = crate::lifecycle::serve(
        configuration.management_address,
        configuration.service.application,
        options,
        configuration.shutdown_timeout,
        deployment,
    )
    .await;
    match result {
        Ok(()) => success(),
        Err(error) => failure(1, format!("service runtime {}", error.kind())),
    }
}

fn check(configuration_path: &std::path::Path, output: &mut impl Write) -> CliResult {
    if let Err(error) = configuration::load(configuration_path) {
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
    if arguments.next().is_some() {
        return Err(());
    }
    Ok(Command::Check { configuration })
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
    "usage: uob serve --config PATH [--no-ui] | uob config check --config PATH | uob events [--config PATH] [--after CURSOR] --format jsonl".to_owned()
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
    }
}
