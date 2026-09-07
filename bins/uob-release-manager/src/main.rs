mod artifact_cli;
mod supervisor_cli;

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let result = if args.len() == 1 && args[0] == "--version" {
        println!("uob-release-manager {}", env!("CARGO_PKG_VERSION"));
        Ok(())
    } else if args.first().is_some_and(|arg| arg == "serve") {
        if args.len() == 2 {
            supervisor_cli::run(std::path::Path::new(&args[1]))
        } else {
            Err("usage: uob-release-manager serve CONFIG".into())
        }
    } else {
        artifact_cli::run()
    };
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
