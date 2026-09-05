use std::io::{self, Write};

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("service runtime");
    let stdout = io::stdout();
    let mut output = stdout.lock();
    let result = runtime.block_on(uob_service::cli::execute(
        std::env::args().skip(1),
        &mut output,
    ));
    // Cancel remaining asynchronous connections; bound waits for blocking library work.
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));
    if let Some(diagnostic) = result.diagnostic {
        let _ = writeln!(io::stderr().lock(), "{diagnostic}");
    }
    if result.exit_code != 0 {
        std::process::exit(i32::from(result.exit_code));
    }
}
