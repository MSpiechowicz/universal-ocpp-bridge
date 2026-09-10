use std::{fs, os::unix::fs::PermissionsExt, process::Command};

#[test]
fn real_candidate_checks_production_secret_references_without_exposing_them() {
    let root = std::env::temp_dir().join(format!("uob-preflight-cli-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let config = root.join("production.toml");
    let token = root.join("sensitive-token-path");
    fs::write(
        &config,
        format!(
            "[bridge]\nid='production'\n[events]\ncredentials_file='{}'\n",
            token.display()
        ),
    )
    .unwrap();
    let run = |secrets: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_uob"));
        command.args(["config", "check", "--config"]).arg(&config);
        if secrets {
            command.arg("--secrets");
        }
        command.output().unwrap()
    };
    assert!(run(false).status.success());
    let result = run(true);
    assert_eq!(result.status.code(), Some(2));
    assert!(!String::from_utf8_lossy(&result.stderr).contains("sensitive-token-path"));
    fs::write(&token, "sensitive-token-content").unwrap();
    fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
    let result = run(true);
    assert!(result.status.success());
    assert_eq!(result.stdout, b"{\"status\":\"valid\"}\n");
    assert!(result.stderr.is_empty());
    fs::write(&config, "[bridge]\nid='test'\nenvironment='demo'\n").unwrap();
    assert_eq!(run(true).status.code(), Some(2));
    fs::remove_dir_all(root).unwrap();
}
