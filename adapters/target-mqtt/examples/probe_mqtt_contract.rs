#[path = "../../../tests/ems-mqtt-contract-client/probe.rs"]
mod probe;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    match execute().await {
        Ok(evidence) => println!(
            "{}",
            serde_json::to_string(&evidence).expect("evidence JSON")
        ),
        Err(error) => {
            eprintln!("MQTT contract probe: {error}");
            std::process::exit(1);
        }
    }
}

async fn execute() -> probe::Result<probe::Evidence> {
    let mut args = std::env::args().skip(1);
    let url = args.next().ok_or_else(|| error("missing broker URL"))?;
    let path = args.next().ok_or_else(|| error("missing scenario path"))?;
    let mut exercise = false;
    let mut allow_remote = false;
    for argument in args {
        match argument.as_str() {
            "--exercise" if !exercise => exercise = true,
            "--allow-remote-exercise" if !allow_remote => allow_remote = true,
            _ => return Err(error("unknown or repeated option")),
        }
    }
    if allow_remote && !exercise {
        return Err(error("--allow-remote-exercise requires --exercise"));
    }
    let scenario = read_scenario(&path)?;
    let demo: probe::Demo = toml::from_str(&scenario).map_err(|_| error("invalid scenario"))?;
    let ca = std::env::var_os("UOB_MQTT_CA_FILE").ok_or_else(|| error("CA file missing"))?;
    let username =
        std::env::var("UOB_MQTT_CLIENT_USER").map_err(|_| error("MQTT username missing"))?;
    let password = std::env::var_os("UOB_MQTT_CLIENT_PASSWORD_FILE")
        .ok_or_else(|| error("MQTT password file missing"))?;
    probe::run(
        &url,
        std::path::Path::new(&ca),
        &username,
        std::path::Path::new(&password),
        &demo,
        exercise,
        allow_remote,
    )
    .await
}

fn error(message: &str) -> probe::Error {
    probe::Error::from_message(message)
}

fn read_scenario(path: &str) -> probe::Result<String> {
    use std::io::Read;
    const LIMIT: u64 = 64 * 1024;
    let path = std::path::Path::new(path);
    let meta = std::fs::metadata(path).map_err(|_| error("scenario unreadable"))?;
    if !meta.is_file() || meta.len() > LIMIT {
        return Err(error("scenario not regular or exceeds bound"));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|_| error("scenario unreadable"))?
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| error("scenario unreadable"))?;
    if bytes.len() as u64 > LIMIT {
        return Err(error("scenario exceeds bound"));
    }
    String::from_utf8(bytes).map_err(|_| error("scenario is not UTF-8"))
}
