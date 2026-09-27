use std::{collections::BTreeMap, fs, path::Path};

use super::Package;

const OWNER: &str = "uob-postgresql-export-adapter";
const DRIVERS: &[&str] = &[
    "tokio-postgres",
    "postgres",
    "postgres-types",
    "postgres-protocol",
    "tokio-postgres-rustls",
    "postgres_rustls",
    "postgres-native-tls",
    "postgres-openssl",
];
const POOLS: &[&str] = &[
    "deadpool-postgres",
    "bb8-postgres",
    "r2d2-postgres",
    "mobc-postgres",
];
const GENERIC_POOLS: &[&str] = &["deadpool", "bb8", "r2d2", "mobc"];

pub(super) fn check(packages: &BTreeMap<&str, &Package>, errors: &mut Vec<String>) {
    for (name, package) in packages {
        let mut imports: Vec<String> = DRIVERS
            .iter()
            .chain(POOLS)
            .map(|name| name.replace('-', "_"))
            .collect();

        for dependency in &package.dependencies {
            let dependency_name = dependency.name.as_str();
            let is_pool = POOLS.contains(&dependency_name);
            let is_driver = DRIVERS.contains(&dependency_name);
            let is_generic_pool = GENERIC_POOLS.contains(&dependency_name);

            if is_pool || (*name == OWNER && is_generic_pool) {
                errors.push(format!(
                    "{name} declares prohibited PostgreSQL pool dependency {}",
                    dependency.name
                ));
            } else if is_driver && *name != OWNER {
                errors.push(format!(
                    "{name} declares PostgreSQL driver {}, owned only by {OWNER}",
                    dependency.name
                ));
            }

            if is_pool || is_driver {
                imports.push(
                    dependency
                        .rename
                        .as_deref()
                        .unwrap_or(dependency_name)
                        .replace('-', "_"),
                );
            }
        }

        if *name == OWNER {
            continue;
        }
        let Some(directory) = package.manifest_path.parent() else {
            continue;
        };
        scan_sources(name, directory, &imports, errors);
    }
}

fn scan_sources(package: &str, directory: &Path, imports: &[String], errors: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(directory) else {
        errors.push(format!(
            "could not read PostgreSQL boundary directory {}",
            directory.display()
        ));
        return;
    };
    for entry in entries {
        let Ok(entry) = entry else {
            errors.push(format!(
                "could not enumerate PostgreSQL boundary directory {}",
                directory.display()
            ));
            continue;
        };
        let path = entry.path();
        if path.is_dir() {
            if !matches!(entry.file_name().to_str(), Some("target" | ".git")) {
                scan_sources(package, &path, imports, errors);
            }
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            let Ok(source) = fs::read_to_string(&path) else {
                errors.push(format!(
                    "could not read PostgreSQL boundary source {}",
                    path.display()
                ));
                continue;
            };
            if let Some(import) = forbidden_import(&source, imports) {
                errors.push(format!(
                    "{package} references PostgreSQL driver {import} in {} (owned only by {OWNER})",
                    path.display()
                ));
            }
        }
    }
}

fn forbidden_import<'a>(source: &'a str, imports: &[String]) -> Option<&'a str> {
    let tokens = code_tokens(source);
    for (index, token) in tokens.iter().enumerate() {
        if !imports.iter().any(|name| name == token) {
            continue;
        }
        if tokens.get(index + 1) == Some(&"::") {
            return Some(token);
        }
        if tokens.get(index.wrapping_sub(1)) == Some(&"crate")
            && tokens.get(index.wrapping_sub(2)) == Some(&"extern")
        {
            return Some(token);
        }
        if tokens[..index]
            .iter()
            .rposition(|token| *token == "use" || *token == ";")
            .is_some_and(|start| tokens[start] == "use")
        {
            return Some(token);
        }
    }
    None
}

// Inspect Rust tokens rather than substring-matching documentation, comments, or catalog strings.
fn code_tokens(source: &str) -> Vec<&str> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let start = index;
        match bytes[index] {
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index += 2;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                let mut depth = 1;
                while index + 1 < bytes.len() && depth > 0 {
                    if &bytes[index..index + 2] == b"/*" {
                        depth += 1;
                        index += 2;
                    } else if &bytes[index..index + 2] == b"*/" {
                        depth -= 1;
                        index += 2;
                    } else {
                        index += 1;
                    }
                }
            }
            b'r' if bytes.get(index + 1) == Some(&b'#') || bytes.get(index + 1) == Some(&b'"') => {
                index = raw_string_end(bytes, index).unwrap_or(index + 1);
            }
            b'"' => index = quoted_end(bytes, index, b'"'),
            b'\'' if bytes.get(index + 2) == Some(&b'\'') => {
                index = quoted_end(bytes, index, b'\'');
            }
            b':' if bytes.get(index + 1) == Some(&b':') => {
                tokens.push(&source[index..index + 2]);
                index += 2;
            }
            b';' => {
                tokens.push(&source[index..=index]);
                index += 1;
            }
            byte if byte.is_ascii_alphabetic() || byte == b'_' => {
                index += 1;
                while bytes
                    .get(index)
                    .is_some_and(|next| next.is_ascii_alphanumeric() || *next == b'_')
                {
                    index += 1;
                }
                tokens.push(&source[start..index]);
            }
            _ => index += 1,
        }
    }
    tokens
}

fn quoted_end(bytes: &[u8], start: usize, quote: u8) -> usize {
    let mut index = start + 1;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            index += 2;
        } else if bytes[index] == quote {
            return index + 1;
        } else {
            index += 1;
        }
    }
    index
}

fn raw_string_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut opener = start + 1;
    while bytes.get(opener) == Some(&b'#') {
        opener += 1;
    }
    if bytes.get(opener) != Some(&b'"') {
        return None;
    }
    let hashes = opener - start - 1;
    let mut index = opener + 1;
    while index < bytes.len() {
        if bytes[index] == b'"'
            && bytes.get(index + 1..index + 1 + hashes) == Some(&bytes[start + 1..opener])
        {
            return Some(index + 1 + hashes);
        }
        index += 1;
    }
    Some(bytes.len())
}

#[cfg(test)]
mod tests {
    use super::{code_tokens, forbidden_import};

    #[test]
    fn source_imports_are_detected_without_matching_neutral_text() {
        let imports = vec!["tokio_postgres".to_owned(), "pg_client".to_owned()];
        for source in [
            "use tokio_postgres as pg;",
            "pub use {other::Thing, tokio_postgres as pg};",
            "extern crate pg_client;",
            "let _: tokio_postgres::Client;",
            "pub use pg_client::{Client};",
        ] {
            assert!(forbidden_import(source, &imports).is_some(), "{source}");
        }
        for source in [
            "// use tokio_postgres::Client;",
            "/* pub use tokio_postgres as pg; */",
            "const KIND: &str = \"tokio_postgres::Client\";",
            "const KIND: &str = r#\"use tokio_postgres::Client\"#;",
            "let postgresql = \"postgresql\";",
        ] {
            assert_eq!(forbidden_import(source, &imports), None, "{source}");
        }
        assert_eq!(
            code_tokens("/* outer /* inner */ */ use pg_client;"),
            vec!["use", "pg_client", ";"]
        );
    }
}
