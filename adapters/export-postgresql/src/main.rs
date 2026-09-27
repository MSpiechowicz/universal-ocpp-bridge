mod connection;
mod credentials;
mod errors;
mod inbound;
mod settings;
mod tls;

use std::time::{Duration, Instant};

use connection::{Connection, driver_tasks};
use settings::Settings;
use uob_application::{DatabaseError, DatabaseRetryClassification};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let started = Instant::now();
    let args: Vec<_> = std::env::args().skip(1).collect();
    let result = match Settings::parse(&args) {
        Ok((settings, scenario, marker)) => execute(&settings, scenario, marker).await,
        Err(error) => Err(error),
    };
    match result {
        Ok(()) => println!(
            "outcome=ok elapsed_ms={} driver_tasks={}",
            started.elapsed().as_millis(),
            driver_tasks()
        ),
        Err(error) => {
            println!(
                "outcome=error code={:?} retry={:?} context={} elapsed_ms={} driver_tasks={}",
                error.code(),
                error.retry_classification(),
                error.context(),
                started.elapsed().as_millis(),
                driver_tasks()
            );
            std::process::exit(1);
        }
    }
}

async fn execute(
    settings: &Settings,
    scenario: &str,
    marker: Option<&str>,
) -> Result<(), DatabaseError> {
    if scenario.starts_with("stress") {
        return stress(settings, scenario).await;
    }
    let mut connection = Connection::connect(settings).await?;
    let result = match scenario {
        "probe" => connection.probe().await,
        "sleep" => connection.sleep().await,
        "transaction" => {
            connection
                .transaction(marker.expect("validated marker"))
                .await
        }
        "cancel" => {
            let result =
                tokio::time::timeout(std::time::Duration::from_millis(150), connection.sleep())
                    .await;
            match result {
                Err(_) => Err(errors::unavailable("postgres.probe.cancelled")),
                Ok(result) => result,
            }
        }
        _ => unreachable!("validated scenario"),
    };
    // A cancelled future or failed operation cannot leave a reusable session. The owner
    // always joins the supervised connection task before the next attempt.
    connection.close().await?;
    if driver_tasks() != 0 {
        return Err(errors::shutdown());
    }
    result
}

async fn stress(settings: &Settings, scenario: &str) -> Result<(), DatabaseError> {
    let iterations = if scenario == "stress-recovery" {
        1000
    } else {
        100
    };
    let mut baseline_kib = 0;
    let mut peak_kib = 0;
    let mut peak_sockets = 0;

    for index in 0..iterations {
        if scenario == "stress-recovery" {
            let rejection = Connection::connect(settings).await;
            if !matches!(&rejection, Err(error) if error.retry_classification() == DatabaseRetryClassification::Permanent)
            {
                return Err(errors::invalid("postgres.stress.rejection_missing"));
            }
            if driver_tasks() != 0 || socket_count() != 0 {
                return Err(errors::shutdown());
            }
        }
        let mut connection = Connection::connect(settings).await?;
        peak_sockets = peak_sockets.max(socket_count());
        if driver_tasks() != 1 {
            return Err(errors::shutdown());
        }
        match scenario {
            "stress-cancel" => {
                let result =
                    tokio::time::timeout(Duration::from_millis(150), connection.sleep()).await;
                if result.is_ok() {
                    return Err(errors::invalid("postgres.stress.cancellation_missing"));
                }
            }
            "stress-rollback" => {
                let marker = format!("rollback_{index}");
                let result = connection.transaction(&marker).await;
                if !matches!(&result, Err(error) if error.retry_classification() == DatabaseRetryClassification::Uncertain)
                {
                    return Err(errors::invalid("postgres.stress.uncertainty_missing"));
                }
            }
            _ => connection.probe().await?,
        }
        connection.close().await?;
        let current_kib = rss_kib();
        if index == 0 {
            baseline_kib = current_kib;
        }
        peak_kib = peak_kib.max(current_kib);
        if driver_tasks() != 0 || socket_count() != 0 {
            return Err(errors::shutdown());
        }
    }

    let final_kib = rss_kib();
    let fds = fd_count();
    println!(
        "scenario={scenario} iterations={iterations} baseline_rss_kib={baseline_kib} peak_rss_kib={peak_kib} final_rss_kib={final_kib} fds={fds} peak_sockets={peak_sockets}"
    );
    if peak_sockets == 0
        || peak_sockets > 2
        || peak_kib > 64 * 1024
        || final_kib > baseline_kib.saturating_add(8 * 1024)
        || fds > 16
    {
        return Err(errors::unavailable("postgres.resources.exceeded"));
    }
    Ok(())
}

fn socket_count() -> usize {
    std::fs::read_dir("/proc/self/fd").map_or(0, |entries| {
        entries
            .filter_map(Result::ok)
            .filter(|entry| {
                std::fs::read_link(entry.path())
                    .is_ok_and(|target| target.to_string_lossy().starts_with("socket:["))
            })
            .count()
    })
}

fn rss_kib() -> usize {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                line.strip_prefix("VmRSS:")?
                    .split_whitespace()
                    .next()?
                    .parse()
                    .ok()
            })
        })
        .unwrap_or(0)
}

fn fd_count() -> usize {
    std::fs::read_dir("/proc/self/fd").map_or(0, Iterator::count)
}
