use std::io::{self, Write};

use uob_sim::scenario::execute;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    if arguments.first().map(String::as_str) == Some("serve") {
        if let Err(code) = uob_sim::control::serve_arguments(arguments).await {
            eprintln!("simulator control: {code}");
            std::process::exit(2);
        }
        return;
    }
    let result = execute(arguments).await;
    let stdout = io::stdout();
    let mut output = stdout.lock();
    for event in &result.report.events {
        serde_json::to_writer(&mut output, event).expect("serializing a report event cannot fail");
        writeln!(output).expect("writing a report event to stdout failed");
    }
    if let Some(diagnostic) = result.diagnostic {
        eprintln!("{diagnostic}");
    }
    if result.exit_code != 0 {
        std::process::exit(i32::from(result.exit_code));
    }
}
