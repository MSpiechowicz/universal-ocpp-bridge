use uob_contracts::Environment;

/// Checks the audience and bounded syntax of a management bearer credential.
///
/// This is not authentication: callers must also verify the **entire** token against an
/// independently provisioned secret and apply its normal resource/permission grant. Never strip
/// or rewrite the prefix before verification. Generate a different random secret per environment.
/// Startup must reject copied credentials whose audience differs from the service identity.
#[must_use]
pub fn token_matches_environment(token: &str, environment: Environment) -> bool {
    let prefix = match environment {
        Environment::Production => "uob1.production.",
        Environment::Staging => "uob1.staging.",
        Environment::Demo => "uob1.demo.",
    };
    token.strip_prefix(prefix).is_some_and(|secret| {
        (32..=128).contains(&secret.len()) && secret.bytes().all(|b| b.is_ascii_graphic())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audience_is_exact_and_legacy_or_malformed_tokens_fail_closed() {
        for (name, environment) in [
            ("production", Environment::Production),
            ("staging", Environment::Staging),
            ("demo", Environment::Demo),
        ] {
            let token = format!("uob1.{name}.{}", "x".repeat(32));
            for other in [
                Environment::Production,
                Environment::Staging,
                Environment::Demo,
            ] {
                assert_eq!(
                    token_matches_environment(&token, other),
                    environment == other
                );
            }
            for secret in ["x".repeat(31), "x".repeat(129), " ".repeat(32)] {
                assert!(!token_matches_environment(
                    &format!("uob1.{name}.{secret}"),
                    environment
                ));
            }
            assert!(!token_matches_environment(&"x".repeat(32), environment));
        }
    }
}
