use std::fs;

use super::{
    FailureCategory, RunFailure, RunReport, ScenarioRunner, cancellation_pair, parse_configuration,
    parse_scenario,
};

#[derive(Debug)]
pub struct CliResult {
    pub report: RunReport,
    pub exit_code: u8,
    pub diagnostic: Option<String>,
}

#[derive(Debug)]
struct RunArguments {
    configuration_path: String,
    scenario_path: String,
    seed_override: Option<u64>,
}

pub async fn execute(arguments: impl IntoIterator<Item = String>) -> CliResult {
    let arguments = match parse_arguments(arguments) {
        Ok(arguments) => arguments,
        Err(failure) => return failure_result(0, failure),
    };
    let configuration_text = match read_document(&arguments.configuration_path, "configuration") {
        Ok(document) => document,
        Err(failure) => return failure_result(arguments.seed_override.unwrap_or(0), failure),
    };
    let scenario_text = match read_document(&arguments.scenario_path, "scenario") {
        Ok(document) => document,
        Err(failure) => return failure_result(arguments.seed_override.unwrap_or(0), failure),
    };
    let configuration = match parse_configuration(&configuration_text) {
        Ok(configuration) => configuration,
        Err(failure) => return failure_result(arguments.seed_override.unwrap_or(0), failure),
    };
    let scenario = match parse_scenario(&scenario_text) {
        Ok(scenario) => scenario,
        Err(failure) => return failure_result(arguments.seed_override.unwrap_or(0), failure),
    };
    let seed = arguments.seed_override.unwrap_or(scenario.seed);
    let (cancellation, token) = cancellation_pair();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            cancellation.cancel();
        }
    });
    let report = ScenarioRunner::default()
        .run(&configuration, &scenario, seed, token)
        .await;
    result_from_report(report)
}

fn parse_arguments(
    arguments: impl IntoIterator<Item = String>,
) -> Result<RunArguments, RunFailure> {
    let mut arguments = arguments.into_iter();
    if arguments.next().as_deref() != Some("run") {
        return Err(usage_failure());
    }

    let mut configuration_path = None;
    let mut scenario_path = None;
    let mut seed_override = None;
    let mut format_seen = false;
    while let Some(argument) = arguments.next() {
        let value = arguments.next().ok_or_else(usage_failure)?;
        match argument.as_str() {
            "--config" if configuration_path.is_none() => configuration_path = Some(value),
            "--scenario" if scenario_path.is_none() => scenario_path = Some(value),
            "--seed" if seed_override.is_none() => {
                seed_override = Some(value.parse().map_err(|_| usage_failure())?);
            }
            "--format" if !format_seen && value == "jsonl" => format_seen = true,
            _ => return Err(usage_failure()),
        }
    }

    Ok(RunArguments {
        configuration_path: configuration_path.ok_or_else(usage_failure)?,
        scenario_path: scenario_path.ok_or_else(usage_failure)?,
        seed_override,
    })
    .and_then(|arguments| {
        if format_seen {
            Ok(arguments)
        } else {
            Err(usage_failure())
        }
    })
}

fn read_document(path: &str, kind: &'static str) -> Result<String, RunFailure> {
    fs::read_to_string(path).map_err(|_| {
        RunFailure::new(
            FailureCategory::Setup,
            "document_unavailable",
            format!("{kind} document could not be read"),
        )
    })
}

fn usage_failure() -> RunFailure {
    RunFailure::new(
        FailureCategory::Setup,
        "invalid_arguments",
        "usage: uob-sim run --config PATH --scenario PATH [--seed NUMBER] --format jsonl",
    )
}

fn failure_result(seed: u64, failure: RunFailure) -> CliResult {
    let diagnostic = Some(format!("scenario {}: {}", failure.code, failure.message));
    let exit_code = failure.category.exit_code();
    CliResult {
        report: RunReport::failed(seed, failure),
        exit_code,
        diagnostic,
    }
}

fn result_from_report(report: RunReport) -> CliResult {
    let (exit_code, diagnostic) = report.failure.as_ref().map_or((0, None), |failure| {
        (
            failure.category.exit_code(),
            Some(format!("scenario {}: {}", failure.code, failure.message)),
        )
    });
    CliResult {
        report,
        exit_code,
        diagnostic,
    }
}
