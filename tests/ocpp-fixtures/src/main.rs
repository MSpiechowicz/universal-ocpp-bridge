use std::path::PathBuf;

use uob_ocpp_fixtures::{CheckMode, check_corpus, corpus_root};

fn main() {
    let mut mode = CheckMode::Development;
    let mut root: Option<PathBuf> = None;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--release" => mode = CheckMode::Release,
            "--root" => {
                root = Some(PathBuf::from(
                    arguments.next().expect("--root requires a path"),
                ));
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }

    match check_corpus(&root.unwrap_or_else(corpus_root), mode) {
        Ok(report) => println!(
            "validated {} fixtures and {} requirements ({} verified, {} required remaining)",
            report.fixtures,
            report.requirements,
            report.verified_requirements,
            report.required_remaining
        ),
        Err(errors) => {
            for error in errors {
                eprintln!("{error}");
            }
            std::process::exit(1);
        }
    }
}
