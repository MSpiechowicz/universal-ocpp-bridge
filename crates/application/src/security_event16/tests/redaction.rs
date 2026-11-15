use crate::security_event16::{REDACTION_MARKER, redact_tech_info_16};

mod property;

fn redacted(raw: &str) -> (String, u16) {
    let result = redact_tech_info_16(raw);
    (result.text, result.redactions)
}

/// Each `(input, output)` pair is redacted exactly once.
fn assert_redacted_once(cases: &[(&str, &str)]) {
    for (raw, expected) in cases {
        assert_eq!(redacted(raw), ((*expected).to_owned(), 1), "{raw}");
    }
}

fn assert_unchanged(cases: &[&str]) {
    for text in cases {
        assert_eq!(redacted(text), ((*text).to_owned(), 0), "{text}");
    }
}

#[test]
fn text_without_secrets_is_unchanged() {
    assert_unchanged(&[
        "",
        "Firmware signature check failed for image 3",
        "keyboard attached",
        "monkey: 3",
        "keyboard=1 turkey=2 spin=2 bypass=1",
        "TLS version 1.1 rejected",
        "id 123e4567-e89b-12d3-a456-426614174000",
        "TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384",
        "ünïcode Ωmega — no secrets, 日本語",
        "user=svc-demo origin=touchscreen",
        "Authorization header missing",
    ]);
}

#[test]
fn a_labelled_value_is_replaced_and_the_context_is_kept() {
    assert_redacted_once(&[
        (
            "login failed: Password: hunter2 for user alice",
            "login failed: Password: [redacted] for user alice",
        ),
        ("a=1&token = abc123", "a=1&token = [redacted]"),
        ("(pin=1234) done", "(pin=[redacted]) done"),
        ("password=[REDACTED]", "password=[redacted]"),
    ]);
}

#[test]
fn every_label_is_recognised_without_regard_to_case() {
    for label in [
        "password",
        "passwd",
        "pwd",
        "pass",
        "passphrase",
        "secret",
        "token",
        "authorizationkey",
        "AuthorizationKey",
        "authkey",
        "apikey",
        "api_key",
        "key",
        "credential",
        "credentials",
        "idtag",
        "idTag",
        "id_tag",
        "idtoken",
        "id_token",
        "pin",
        "otp",
        "totp",
        "privatekey",
        "private_key",
        "PASSWORD",
    ] {
        assert_eq!(
            redacted(&format!("x {label}:s3cr3t y")),
            (format!("x {label}:[redacted] y"), 1),
            "{label}"
        );
    }
}

#[test]
fn a_label_at_the_end_of_an_identifier_is_recognised() {
    assert_redacted_once(&[
        ("dbPassword=hunter2", "dbPassword=[redacted]"),
        ("userPassword: x1", "userPassword: [redacted]"),
        ("mypassword=x1", "mypassword=[redacted]"),
        ("adminpwd=x1", "adminpwd=[redacted]"),
        ("wifiPassword=x1", "wifiPassword=[redacted]"),
        ("BasicAuthPassword=x1", "BasicAuthPassword=[redacted]"),
        ("brokerToken=x1", "brokerToken=[redacted]"),
        ("x_secret=x1", "x_secret=[redacted]"),
        ("db.password=x1", "db.password=[redacted]"),
        ("x-api-key: y2", "x-api-key: [redacted]"),
        ("my_key=z3", "my_key=[redacted]"),
        ("MAINT_AUTHKEY=z3", "MAINT_AUTHKEY=[redacted]"),
        ("SECRETKEY=z3", "SECRETKEY=[redacted]"),
        ("accesskey: z3", "accesskey: [redacted]"),
        ("encryptionkey=z3", "encryptionkey=[redacted]"),
        ("SIMPIN=1234", "SIMPIN=[redacted]"),
        ("wifikey=z3", "wifikey=[redacted]"),
        ("psk=z3", "psk=[redacted]"),
    ]);
}

#[test]
fn a_short_label_needs_a_word_boundary() {
    assert_unchanged(&[
        "monkey: 3",
        "turkey=5",
        "keyboard=1",
        "spin=2",
        "bypass=1",
        "passport: ok",
        "pinned=yes",
        "hotkey: F5",
        "compass=1",
        "secretary=bob",
        "key without separator",
        "key",
    ]);

    // Start of text, a non-alphanumeric character or a camelCase step is a boundary.
    assert_redacted_once(&[
        ("key=a1", "key=[redacted]"),
        ("apiKey=a1", "apiKey=[redacted]"),
        ("userPin: 1234", "userPin: [redacted]"),
        ("sshKey=a1", "sshKey=[redacted]"),
        ("SSHKey=a1", "SSHKey=[redacted]"),
        ("my pin: 12", "my pin: [redacted]"),
    ]);
}

#[test]
fn a_trailing_plural_or_number_does_not_hide_a_label() {
    assert_redacted_once(&[
        ("tokens=abc", "tokens=[redacted]"),
        ("passwords: x1", "passwords: [redacted]"),
        ("password1=abc", "password1=[redacted]"),
        ("secret2=abc", "secret2=[redacted]"),
        ("tokens2: abc", "tokens2: [redacted]"),
        ("pins=1234", "pins=[redacted]"),
        ("dbPasswords1 = abc", "dbPasswords1 = [redacted]"),
    ]);
    assert_unchanged(&["monkeys: 3", "spins=2", "tokenizer=on"]);
}

#[test]
fn quoted_values_run_to_the_closing_quote() {
    assert_redacted_once(&[
        (
            r#"{"user":"alice","password":"hunter2hunter2"}"#,
            r#"{"user":"alice","password":"[redacted]"}"#,
        ),
        (
            r#"{"key" : "two words, with: punctuation" , "n": 1}"#,
            r#"{"key" : "[redacted]" , "n": 1}"#,
        ),
        ("secret='it is' ok", "secret='[redacted]' ok"),
        // An unterminated quote swallows the rest, never leaking it.
        (r#"pwd="abc def"#, r#"pwd="[redacted]"#),
    ]);
}

#[test]
fn a_backslash_escapes_a_quote_inside_a_quoted_value() {
    assert_redacted_once(&[
        (r#"{"password":"ab\"cd"}"#, r#"{"password":"[redacted]"}"#),
        (r"secret='ab\'cd' ok", r"secret='[redacted]' ok"),
        (r#"{"token":"ab\\"} tail"#, r#"{"token":"[redacted]"} tail"#),
        // JSON text carried inside a JSON string.
        (
            r#"{\"password\":\"abcd\",\"user\":\"alice\"}"#,
            r#"{\"password\":\"[redacted]\",\"user\":\"alice\"}"#,
        ),
        (
            r#"{\"password\":\"ab\\\"cd\"}"#,
            r#"{\"password\":\"[redacted]\"}"#,
        ),
    ]);
}

#[test]
fn an_empty_value_has_nothing_to_redact() {
    assert_unchanged(&[
        "password=",
        "password= ",
        r#"password="""#,
        "token:,next",
        "authorization: ",
    ]);
}

#[test]
fn alternative_and_url_encoded_separators_are_recognised() {
    assert_redacted_once(&[
        ("password => hunter2", "password => [redacted]"),
        ("password := hunter2", "password := [redacted]"),
        ("password -> hunter2", "password -> [redacted]"),
        ("password->hunter2", "password->[redacted]"),
        ("password == hunter2", "password == [redacted]"),
        ("password%3Dhunter2", "password%3D[redacted]"),
        ("password%3ahunter2 ok", "password%3a[redacted] ok"),
        ("PASSWORD%3dhunter2", "PASSWORD%3d[redacted]"),
        (
            "user=a&dbPassword%3Ahunter2",
            "user=a&dbPassword%3A[redacted]",
        ),
    ]);
}

#[test]
fn an_authorization_value_is_replaced_as_a_whole() {
    assert_redacted_once(&[
        ("Authorization: opaque-token-1", "Authorization: [redacted]"),
        ("Authorization: Basic abc123", "Authorization: [redacted]"),
        (
            "Authorization: Digest zz9 failed",
            "Authorization: [redacted] failed",
        ),
        (
            "proxy-authorization: Bearer abc.def",
            "proxy-authorization: [redacted]",
        ),
        (
            "Proxy-Authorization=Negotiate q1",
            "Proxy-Authorization=[redacted]",
        ),
        (
            r#"{"Authorization":"Bearer abc def"}"#,
            r#"{"Authorization":"[redacted]"}"#,
        ),
        (
            "Authorization: abc, next",
            "Authorization: [redacted], next",
        ),
    ]);
}

#[test]
fn basic_and_bearer_tokens_are_replaced() {
    assert_redacted_once(&[
        ("sent Basic ZmFrZXVzZXI6", "sent Basic [redacted]"),
        (
            "sent bearer abc123, retrying",
            "sent bearer [redacted], retrying",
        ),
        // A labelled scheme word is replaced together with its token.
        ("token: Bearer abc.def next", "token: [redacted] next"),
    ]);
    assert_unchanged(&["Basically fine, Bearers wanted"]);
}

#[test]
fn url_userinfo_is_replaced_and_the_host_is_kept() {
    assert_redacted_once(&[
        (
            "fetch https://fakeuser:fakepass@files.example.test:8443/fw.bin now",
            "fetch https://[redacted]@files.example.test:8443/fw.bin now",
        ),
        // The last `@` before the path ends the userinfo.
        ("ftp://user:p@ss@host/dir", "ftp://[redacted]@host/dir"),
    ]);
    // An `@` in the path or after whitespace is not userinfo.
    assert_unchanged(&[
        "https://host/path@elsewhere",
        "https://host mail me@example.test",
        "https://host",
    ]);
}

#[test]
fn long_hex_and_mixed_case_base64_runs_are_replaced() {
    let hex40 = "0123456789abcdef0123456789abcdef01234567";
    assert_eq!(
        redacted(&format!("old {hex40} rejected")),
        ("old [redacted] rejected".to_owned(), 1)
    );
    assert_eq!(
        redacted(&"A1".repeat(16)),
        (REDACTION_MARKER.to_owned(), 1),
        "32 hex digits in upper case"
    );

    let base64 = "dGhpcyBpcyBhIFNlY3JldC9LZXkrMTIzNDU2Nzg5MA==";
    assert_eq!(
        redacted(&format!("blob={base64}.")),
        ("blob=[redacted].".to_owned(), 1)
    );
    assert_eq!(
        redacted(&format!("blob {base64}.")),
        ("blob [redacted].".to_owned(), 1)
    );

    // Shorter, or missing a character class, stays.
    assert_unchanged(&[
        "0123456789abcdef0123456789abcde",
        "AbCdEfGhIjKlMnOpQrStUv1",
        "abcdefghijklmnopqrstuvwxyz",
        "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123",
    ]);
}

#[test]
fn several_secrets_are_all_replaced_and_counted() {
    assert_eq!(
        redacted("password=a1 Basic Zm9vOmJhcg== https://u:p@h/x token:t2"),
        (
            "password=[redacted] Basic [redacted] https://[redacted]@h/x token:[redacted]"
                .to_owned(),
            4
        )
    );
}

/// The `techInfo` of `wire/1.6/security-event-tech-info-credential.json`; both values are fake.
#[test]
fn the_credential_fixture_text_loses_both_secrets_and_keeps_its_context() {
    let raw = "user=svc-demo origin=touchscreen password=not-a-real-password-0000 \
               header=Authorization: Basic ZmFrZXVzZXI6ZmFrZXBhc3N3b3Jk";

    assert_eq!(
        redacted(raw),
        (
            "user=svc-demo origin=touchscreen password=[redacted] header=Authorization: [redacted]"
                .to_owned(),
            2
        )
    );
}

#[test]
fn redacting_redacted_text_changes_nothing() {
    for raw in [
        "password=[REDACTED]",
        "Authorization: Basic ZmFrZXVzZXI6ZmFrZXBhc3N3b3Jk",
        "Authorization: opaque next words",
        r#"{"user":"alice","password":"hunter2hunter2"}"#,
        r#"{"password":"ab\"cd"} dbToken%3Dabc"#,
        r#"{\"password\":\"ab\"} {\"token\":\"unterminated"#,
        "https://fakeuser:fakepass@files.example.test/fw.bin",
        "0123456789abcdef0123456789abcdef01234567",
        "token: Bearer abc.def password=[REDACTED]",
        r#"{"key":"[redacted]x","pwd":"abc"#,
        "secret=[REDACTED] Basic [redacted]x https://[redacted]x@h",
    ] {
        let first = redact_tech_info_16(raw);
        let second = redact_tech_info_16(&first.text);

        assert!(first.redactions > 0, "{raw}");
        assert_eq!(second.text, first.text, "{raw}");
        assert_eq!(second.redactions, 0, "{raw}");
    }
}

#[test]
fn the_output_is_capped_at_1024_characters_on_a_character_boundary() {
    let raw = format!("{} password=x", "é".repeat(2000));
    let result = redact_tech_info_16(&raw);

    assert_eq!(result.text.chars().count(), 1024);
    assert!(result.text.chars().all(|c| c == 'é'));
    assert_eq!(result.redactions, 1);

    let many = "pin=1 ".repeat(300);
    let result = redact_tech_info_16(&many);
    assert_eq!(result.text.chars().count(), 1024);
    assert_eq!(result.redactions, 300);
}

#[test]
fn the_redaction_count_saturates() {
    let result = redact_tech_info_16(&"pin=1 ".repeat(70_000));

    assert_eq!(result.redactions, u16::MAX);
}

#[test]
fn redaction_is_deterministic() {
    let raw = "password=a b token:c https://u:p@h Basic Zm9v";

    assert_eq!(redact_tech_info_16(raw), redact_tech_info_16(raw));
}
