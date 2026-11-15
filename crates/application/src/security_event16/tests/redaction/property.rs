//! Deterministic property checks over generated `techInfo` text.
use crate::security_event16::redact_tech_info_16;

const SECRET: &str = "zzsecretalpha";
const SECOND_SECRET: &str = "zzsecretbeta";

fn assert_stable_and_bounded(raw: &str) -> String {
    let first = redact_tech_info_16(raw);
    let second = redact_tech_info_16(&first.text);

    assert!(first.text.chars().count() <= 1024, "{raw}");
    assert_eq!(second.text, first.text, "{raw}");
    assert_eq!(second.redactions, 0, "{raw}");

    first.text
}

#[test]
fn a_planted_secret_never_survives_any_label_separator_and_value_shape() {
    const LABELS: [&str; 44] = [
        "password",
        "passwd",
        "pwd",
        "pass",
        "passphrase",
        "secret",
        "token",
        "credential",
        "credentials",
        "authorizationkey",
        "AuthorizationKey",
        "authkey",
        "apikey",
        "api_key",
        "key",
        "pin",
        "otp",
        "totp",
        "privatekey",
        "private_key",
        "idtag",
        "idTag",
        "idtoken",
        "dbPassword",
        "userPassword",
        "mypassword",
        "adminpwd",
        "wifiPassword",
        "BasicAuthPassword",
        "brokerToken",
        "x_secret",
        "apiKey",
        "userPin",
        "sshKey",
        "SSHKey",
        "tokens",
        "passwords",
        "password1",
        "secret2",
        "TOKEN",
        "Password",
        "x-api-key",
        "db.password",
        "tokens2",
    ];
    const SEPARATORS: [&str; 12] = [
        "=", ":", ": ", " = ", " => ", " := ", " -> ", "%3D", "%3a", "\":", "\" : ", "\\\":",
    ];
    const PREFIXES: [&str; 4] = ["", "x ", "{\"", "a=1&"];
    const SUFFIXES: [&str; 3] = ["", " tail", "&x=1}"];

    let values = [
        SECRET.to_owned(),
        format!("\"{SECRET}\""),
        format!("'{SECRET}'"),
        format!("\"{SECRET}\\\"{SECOND_SECRET}\""),
        format!("'{SECRET}\\'{SECOND_SECRET}'"),
    ];

    let mut checked = 0;
    for label in LABELS {
        for separator in SEPARATORS {
            for value in &values {
                for prefix in PREFIXES {
                    for suffix in SUFFIXES {
                        let raw = format!("{prefix}{label}{separator}{value}{suffix}");
                        let text = assert_stable_and_bounded(&raw);

                        assert!(!text.contains(SECRET), "{raw} -> {text}");
                        assert!(!text.contains(SECOND_SECRET), "{raw} -> {text}");
                        checked += 1;
                    }
                }
            }
        }
    }
    assert_eq!(checked, 44 * 12 * 5 * 4 * 3);
}

#[test]
fn a_planted_authorization_credential_never_survives() {
    let mut checked = 0;
    for label in ["Authorization", "authorization", "Proxy-Authorization"] {
        for separator in [": ", "=", "%3A", " => "] {
            for value in [
                SECRET.to_owned(),
                format!("Basic {SECRET}"),
                format!("Bearer {SECRET}"),
                format!("Opaque {SECRET}"),
                format!("\"Bearer {SECRET}\""),
            ] {
                for suffix in ["", ", next", " tail"] {
                    let raw = format!("{label}{separator}{value}{suffix}");
                    let text = assert_stable_and_bounded(&raw);

                    assert!(!text.contains(SECRET), "{raw} -> {text}");
                    checked += 1;
                }
            }
        }
    }
    assert_eq!(checked, 3 * 4 * 5 * 3);
}

#[test]
fn arbitrary_fragment_combinations_never_panic_and_stay_stable() {
    let hex = "0123456789abcdef0123456789abcdef".to_owned();
    let fragments = [
        "password",
        "key",
        "Authorization",
        "Basic",
        "token",
        "pin",
        "=",
        ":",
        "->",
        "%3D",
        " ",
        "\"",
        "'",
        "\\",
        ",",
        ".",
        "}",
        "[redacted]",
        "://",
        "u:p@h",
        "é",
        "日",
        "1",
        "s",
        "ab",
        &hex,
    ];

    let mut checked = 0;
    for first in fragments {
        for second in fragments {
            for third in fragments {
                let raw = format!("{first}{second}{third}");
                let _ = assert_stable_and_bounded(&raw);
                checked += 1;
            }
        }
    }
    assert_eq!(checked, fragments.len().pow(3));
}
