mod artifact_cli;

fn main() {
    if let Err(error) = artifact_cli::run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
