use std::io::{self, Write};

#[tokio::main]
async fn main() {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    let result = uob_service::cli::execute(std::env::args().skip(1), &mut output).await;
    if let Some(diagnostic) = result.diagnostic {
        let _ = writeln!(io::stderr().lock(), "{diagnostic}");
    }
    if result.exit_code != 0 {
        std::process::exit(i32::from(result.exit_code));
    }
}
