#[path = "../../../tests/ems-http-contract-client/probe.rs"]
mod probe;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let result = execute().await;
    match result {
        Ok(value) => println!("{value}"),
        Err(code) => {
            println!("{}", serde_json::json!({"status":"failed","error":code}));
            std::process::exit(1);
        }
    }
}

async fn execute() -> Result<serde_json::Value, &'static str> {
    let (base, path, exercise, allow_remote) = parse_args(std::env::args().skip(1))?;
    let source = read_scenario(&path)?;
    let demo: probe::Demo = toml::from_str(&source).map_err(|_| "invalid scenario")?;
    let reader = std::env::var("UOB_EMS_TOKEN").map_err(|_| "reader credential missing")?;
    if exercise {
        let operator =
            std::env::var("UOB_EMS_OPERATOR_TOKEN").map_err(|_| "operator credential missing")?;
        let evidence = probe::exercise::run(&base, &reader, &operator, &demo, allow_remote)
            .await
            .map_err(|error| error.0)?;
        serde_json::to_value(evidence).map_err(|_| "result encoding failed")
    } else {
        let calls = probe::run(&base, &reader, &demo)
            .await
            .map_err(|_| "contract probe failed")?;
        Ok(serde_json::json!({"status":"passed","validated_calls":calls}))
    }
}

fn parse_args(
    mut args: impl Iterator<Item = String>,
) -> Result<(String, String, bool, bool), &'static str> {
    let base = args.next().ok_or("missing API base")?;
    let path = args.next().ok_or("missing scenario path")?;
    let mut exercise = false;
    let mut allow_remote = false;
    for arg in args {
        match arg.as_str() {
            "--exercise" if !exercise => exercise = true,
            "--allow-remote-exercise" if !allow_remote => allow_remote = true,
            _ => return Err("invalid mode"),
        }
    }
    if allow_remote && !exercise {
        return Err("--allow-remote-exercise requires --exercise");
    }
    Ok((base, path, exercise, allow_remote))
}

fn read_scenario(path: &str) -> Result<String, &'static str> {
    use std::io::Read;
    const LIMIT: u64 = 64 * 1024;
    let metadata = std::fs::metadata(path).map_err(|_| "scenario unreadable")?;
    if !metadata.is_file() {
        return Err("scenario must be a regular file");
    }
    if metadata.len() > LIMIT {
        return Err("scenario exceeds bound");
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|_| "scenario unreadable")?;
    let metadata = file.metadata().map_err(|_| "scenario unreadable")?;
    if !metadata.is_file() {
        return Err("scenario must be a regular file");
    }
    if metadata.len() > LIMIT {
        return Err("scenario exceeds bound");
    }
    let mut bytes = Vec::new();
    file.take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "scenario unreadable")?;
    if bytes.len() as u64 > LIMIT {
        return Err("scenario exceeds bound");
    }
    String::from_utf8(bytes).map_err(|_| "invalid scenario")
}

#[cfg(test)]
mod tests {
    use super::{parse_args, read_scenario};

    #[test]
    fn rejects_oversized_scenario_before_loading_and_nonregular_path() {
        let path = std::env::temp_dir().join(format!("uob-issue87-{}.toml", uuid::Uuid::new_v4()));
        std::fs::write(&path, vec![b' '; 64 * 1024 + 1]).unwrap();
        assert_eq!(
            read_scenario(path.to_str().unwrap()).unwrap_err(),
            "scenario exceeds bound"
        );
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            read_scenario(std::env::temp_dir().to_str().unwrap()).unwrap_err(),
            "scenario must be a regular file"
        );
    }

    #[test]
    fn exercise_options_are_explicit_and_order_independent() {
        let parse = |tail: &[&str]| {
            parse_args(
                ["https://ems.example", "scenario.toml"]
                    .into_iter()
                    .chain(tail.iter().copied())
                    .map(str::to_owned),
            )
        };
        assert_eq!(
            parse(&[]).unwrap(),
            (
                "https://ems.example".into(),
                "scenario.toml".into(),
                false,
                false
            )
        );
        assert_eq!(parse(&["--exercise"]).unwrap().2, true);
        for tail in [
            ["--exercise", "--allow-remote-exercise"],
            ["--allow-remote-exercise", "--exercise"],
        ] {
            assert_eq!(parse(&tail).unwrap().3, true);
        }
        for tail in [
            &["--allow-remote-exercise"][..],
            &["--exercise", "--exercise"],
            &[
                "--exercise",
                "--allow-remote-exercise",
                "--allow-remote-exercise",
            ],
            &["--exercise", "extra"],
        ] {
            assert!(parse(tail).is_err(), "{tail:?}");
        }
    }
}
