use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use jsonschema::Draft;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckMode {
    Development,
    Release,
}

#[derive(Debug, Eq, PartialEq)]
pub struct CheckReport {
    pub fixtures: usize,
    pub requirements: usize,
    pub verified_requirements: usize,
    pub required_remaining: usize,
}

#[derive(Deserialize)]
struct Inventory {
    requirements: Vec<Requirement>,
}

#[derive(Deserialize)]
struct Requirement {
    id: String,
    feature: String,
    protocol_version: String,
    direction: String,
    required: bool,
    applicable: bool,
}

#[derive(Deserialize)]
struct Coverage {
    rows: Vec<CoverageRow>,
}

#[derive(Deserialize)]
struct CoverageRow {
    requirement_id: String,
    feature: String,
    protocol_version: String,
    direction: String,
    fixture_ids: Vec<String>,
    scenario_ids: Vec<String>,
    expected_observable_behavior: String,
    status: String,
    evidence: Vec<String>,
}

#[derive(Deserialize)]
struct FixtureManifest {
    fixtures: Vec<Fixture>,
}

#[derive(Deserialize)]
struct Fixture {
    id: String,
    protocol_version: String,
    direction: String,
    action: String,
    message_type: u64,
    schema: String,
    schema_sha256: String,
    wire: String,
    wire_sha256: String,
    authorship: String,
}

#[derive(Deserialize)]
struct Provenance {
    sources: Vec<Source>,
}

#[derive(Deserialize)]
struct Source {
    protocol_version: String,
    revision: String,
    download_url: String,
    archive_sha256: String,
    schema_draft: String,
    license: String,
}

#[must_use]
pub fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus")
}

/// Validates pinned schema provenance, static wire fixtures, and coverage integrity.
///
/// # Errors
///
/// Returns every detected integrity or completeness error so CI presents actionable evidence in
/// one run. Release mode additionally rejects every applicable required row not yet verified by
/// executable project-owned evidence.
pub fn check_corpus(root: &Path, mode: CheckMode) -> Result<CheckReport, Vec<String>> {
    let mut errors = Vec::new();
    let inventory: Inventory = read_json(root, "inventory.json");
    let coverage: Coverage = read_json(root, "coverage.json");
    let fixtures: FixtureManifest = read_json(root, "fixtures.json");
    let provenance: Provenance = read_json(root, "provenance.json");

    validate_provenance(&provenance, &mut errors);
    let fixture_index = validate_fixtures(root, &fixtures.fixtures, &mut errors);
    let requirement_index = unique_index(
        &inventory.requirements,
        |requirement| requirement.id.as_str(),
        "requirement",
        &mut errors,
    );
    let row_index = unique_index(
        &coverage.rows,
        |row| row.requirement_id.as_str(),
        "coverage row",
        &mut errors,
    );

    for requirement in &inventory.requirements {
        let Some(row) = row_index.get(requirement.id.as_str()) else {
            errors.push(format!("missing coverage row for {}", requirement.id));
            continue;
        };
        validate_row(requirement, row, &fixture_index, mode, &mut errors);
    }
    for row in &coverage.rows {
        if !requirement_index.contains_key(row.requirement_id.as_str()) {
            errors.push(format!(
                "coverage row {} has no requirement inventory entry",
                row.requirement_id
            ));
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    let verified_requirements = coverage
        .rows
        .iter()
        .filter(|row| row.status == "verified")
        .count();
    let required_remaining = inventory
        .requirements
        .iter()
        .filter(|requirement| requirement.required && requirement.applicable)
        .filter(|requirement| {
            row_index
                .get(requirement.id.as_str())
                .is_none_or(|row| row.status != "verified")
        })
        .count();

    Ok(CheckReport {
        fixtures: fixtures.fixtures.len(),
        requirements: inventory.requirements.len(),
        verified_requirements,
        required_remaining,
    })
}

fn validate_provenance(provenance: &Provenance, errors: &mut Vec<String>) {
    let mut versions = HashSet::new();
    for source in &provenance.sources {
        if !versions.insert(source.protocol_version.as_str()) {
            errors.push(format!(
                "duplicate provenance for OCPP {}",
                source.protocol_version
            ));
        }
        if source.revision.trim().is_empty()
            || !source
                .download_url
                .starts_with("https://openchargealliance.org/")
            || !valid_digest(&source.archive_sha256)
            || !matches!(source.schema_draft.as_str(), "draft4" | "draft6")
            || !source.license.contains("CC BY-ND 4.0")
        {
            errors.push(format!(
                "OCPP {} provenance is incomplete or unpinned",
                source.protocol_version
            ));
        }
    }
    for required in ["1.6", "2.0.1"] {
        if !versions.contains(required) {
            errors.push(format!("missing provenance for OCPP {required}"));
        }
    }
}

fn validate_fixtures<'a>(
    root: &Path,
    fixtures: &'a [Fixture],
    errors: &mut Vec<String>,
) -> HashMap<&'a str, &'a Fixture> {
    let index = unique_index(fixtures, |fixture| fixture.id.as_str(), "fixture", errors);
    for fixture in fixtures {
        if fixture.authorship != "independently_authored" {
            errors.push(format!(
                "fixture {} is not independently authored",
                fixture.id
            ));
        }
        let Some(schema_path) = safe_join(root, &fixture.schema, errors) else {
            continue;
        };
        let Some(wire_path) = safe_join(root, &fixture.wire, errors) else {
            continue;
        };
        let Some(schema_bytes) = read_bytes(&schema_path, errors) else {
            continue;
        };
        let Some(wire_bytes) = read_bytes(&wire_path, errors) else {
            continue;
        };
        check_digest(
            &fixture.schema,
            &schema_bytes,
            &fixture.schema_sha256,
            errors,
        );
        check_digest(&fixture.wire, &wire_bytes, &fixture.wire_sha256, errors);

        let schema = parse_json(&schema_bytes, &fixture.schema, errors);
        let wire = parse_json(&wire_bytes, &fixture.wire, errors);
        if let (Some(schema), Some(wire)) = (schema, wire) {
            validate_wire(fixture, &schema, &wire, errors);
        }
    }
    index
}

fn validate_wire(fixture: &Fixture, schema: &Value, wire: &Value, errors: &mut Vec<String>) {
    let Some(frame) = wire.as_array() else {
        errors.push(format!("fixture {} is not an OCPP-J array", fixture.id));
        return;
    };
    if frame.len() != 4
        || frame.first().and_then(Value::as_u64) != Some(fixture.message_type)
        || frame.get(2).and_then(Value::as_str) != Some(fixture.action.as_str())
    {
        errors.push(format!(
            "fixture {} envelope does not match its registry entry",
            fixture.id
        ));
        return;
    }
    let Some(payload) = frame.get(3) else {
        errors.push(format!("fixture {} has no request payload", fixture.id));
        return;
    };
    let draft = match schema.get("$schema").and_then(Value::as_str) {
        Some(value) if value.contains("draft-04") => Draft::Draft4,
        Some(value) if value.contains("draft-06") => Draft::Draft6,
        _ => {
            errors.push(format!(
                "fixture {} uses an unpinned schema draft",
                fixture.id
            ));
            return;
        }
    };
    match jsonschema::options().with_draft(draft).build(schema) {
        Ok(validator) => {
            for error in validator.iter_errors(payload) {
                errors.push(format!(
                    "fixture {} payload fails {} at {}: {}",
                    fixture.id, fixture.schema, error.instance_path, error
                ));
            }
        }
        Err(error) => errors.push(format!(
            "schema {} cannot compile for fixture {}: {error}",
            fixture.schema, fixture.id
        )),
    }
}

fn validate_row(
    requirement: &Requirement,
    row: &CoverageRow,
    fixtures: &HashMap<&str, &Fixture>,
    mode: CheckMode,
    errors: &mut Vec<String>,
) {
    if row.feature != requirement.feature
        || row.protocol_version != requirement.protocol_version
        || row.direction != requirement.direction
    {
        errors.push(format!(
            "coverage metadata differs from inventory for {}",
            requirement.id
        ));
    }
    if row.expected_observable_behavior.trim().is_empty() {
        errors.push(format!(
            "{} has no expected observable behavior",
            requirement.id
        ));
    }
    if !row.scenario_ids.is_empty() {
        errors.push(format!(
            "{} references scenario evidence before a scenario registry exists",
            requirement.id
        ));
    }
    for fixture_id in &row.fixture_ids {
        match fixtures.get(fixture_id.as_str()) {
            Some(fixture)
                if fixture.protocol_version == requirement.protocol_version
                    && fixture.direction == requirement.direction => {}
            Some(_) => errors.push(format!(
                "{} references fixture {fixture_id} for another version or direction",
                requirement.id
            )),
            None => errors.push(format!(
                "{} references unknown fixture {fixture_id}",
                requirement.id
            )),
        }
    }

    match row.status.as_str() {
        "verified" => {
            if row.fixture_ids.is_empty() {
                errors.push(format!(
                    "{} is complete without executable fixture evidence",
                    requirement.id
                ));
            }
            for fixture_id in &row.fixture_ids {
                let evidence = format!("fixture:{fixture_id}");
                if !row.evidence.contains(&evidence) {
                    errors.push(format!(
                        "{} omits executable evidence {evidence}",
                        requirement.id
                    ));
                }
            }
        }
        "not_applicable" => {
            if requirement.applicable
                || !row.fixture_ids.is_empty()
                || !row
                    .evidence
                    .iter()
                    .any(|item| item.starts_with("rationale:"))
            {
                errors.push(format!(
                    "{} has an invalid not-applicable classification",
                    requirement.id
                ));
            }
        }
        "planned" | "external_subset" => {}
        other => errors.push(format!("{} has unknown status {other}", requirement.id)),
    }

    if mode == CheckMode::Release
        && requirement.required
        && requirement.applicable
        && row.status != "verified"
    {
        errors.push(format!(
            "release coverage incomplete: {} is {}",
            requirement.id, row.status
        ));
    }
}

fn unique_index<'a, T>(
    values: &'a [T],
    key: impl Fn(&'a T) -> &'a str,
    label: &str,
    errors: &mut Vec<String>,
) -> HashMap<&'a str, &'a T> {
    let mut index = HashMap::new();
    for value in values {
        let id = key(value);
        if index.insert(id, value).is_some() {
            errors.push(format!("duplicate {label} ID {id}"));
        }
    }
    index
}

fn safe_join(root: &Path, relative: &str, errors: &mut Vec<String>) -> Option<PathBuf> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        errors.push(format!(
            "fixture path is not a safe relative path: {relative}"
        ));
        return None;
    }
    Some(root.join(path))
}

fn read_json<T: for<'de> Deserialize<'de>>(root: &Path, relative: &str) -> T {
    let path = root.join(relative);
    let bytes =
        fs::read(&path).unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("cannot parse {}: {error}", path.display()))
}

fn read_bytes(path: &Path, errors: &mut Vec<String>) -> Option<Vec<u8>> {
    match fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(error) => {
            errors.push(format!("cannot read {}: {error}", path.display()));
            None
        }
    }
}

fn parse_json(bytes: &[u8], label: &str, errors: &mut Vec<String>) -> Option<Value> {
    match serde_json::from_slice(bytes) {
        Ok(value) => Some(value),
        Err(error) => {
            errors.push(format!("{label} is malformed JSON: {error}"));
            None
        }
    }
}

fn check_digest(label: &str, bytes: &[u8], expected: &str, errors: &mut Vec<String>) {
    let actual = sha256_hex(bytes);
    if actual != expected {
        errors.push(format!(
            "checksum changed for {label}: expected {expected}, got {actual}"
        ));
    }
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
