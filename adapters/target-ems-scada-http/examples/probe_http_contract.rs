#[path = "../../../tests/ems-http-contract-client/probe.rs"]
mod probe;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut args = std::env::args().skip(1);
    let base = args
        .next()
        .ok_or("usage: probe_http_contract BASE_URL SCENARIO_TOML; set UOB_EMS_TOKEN")?;
    let scenario = args.next().ok_or("missing scenario TOML")?;
    let token = std::env::var("UOB_EMS_TOKEN")?;
    let demo = toml::from_str(&std::fs::read_to_string(scenario)?)?;
    let calls = probe::run(&base, &token, &demo).await?;
    println!("{{\"status\":\"passed\",\"validated_calls\":{calls}}}");
    Ok(())
}
