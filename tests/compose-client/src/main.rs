mod config;
mod evidence;
mod http;
mod mqtt;

use config::{Args, Mode};

#[derive(Debug)]
pub struct Error(pub &'static str);
impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}
impl std::error::Error for Error {}
type Result<T, E = Error> = std::result::Result<T, E>;

#[tokio::main]
async fn main() {
    let result = async {
        let args = Args::parse()?;
        rustls::crypto::aws_lc_rs::default_provider()
            .install_default()
            .map_err(|_| Error("TLS crypto provider unavailable"))?;
        tokio::time::timeout(args.timeout, async {
            match args.mode {
                Mode::Http => http::run(&args).await,
                Mode::Mqtt | Mode::EmsMqtt => mqtt::run(&args).await,
            }
        })
        .await
        .map_err(|_| Error("Compose client evidence deadline exceeded"))?
    }
    .await;
    if let Err(error) = result {
        eprintln!("charging evidence failed: {error}");
        std::process::exit(1);
    }
}
