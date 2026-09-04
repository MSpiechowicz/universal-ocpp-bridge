use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::Value;
use uob_ocpp_fixtures::{CheckMode, check_corpus, corpus_root, sha256_hex};

static TEMP_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

struct TempCorpus {
    root: PathBuf,
}

impl TempCorpus {
    fn copy() -> Self {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("uob-ocpp-corpus-{}-{sequence}", std::process::id()));
        copy_directory(&corpus_root(), &root);
        Self { root }
    }

    fn json(&self, relative: &str) -> Value {
        serde_json::from_slice(&fs::read(self.root.join(relative)).unwrap()).unwrap()
    }

    fn write_json(&self, relative: &str, value: &Value) {
        let mut bytes = serde_json::to_vec_pretty(value).unwrap();
        bytes.push(b'\n');
        fs::write(self.root.join(relative), bytes).unwrap();
    }

    fn update_wire_digest(&self, fixture_id: &str, wire: &[u8]) {
        let mut manifest = self.json("fixtures.json");
        let fixture = manifest["fixtures"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|fixture| fixture["id"] == fixture_id)
            .unwrap();
        fixture["wire_sha256"] = Value::String(sha256_hex(wire));
        self.write_json("fixtures.json", &manifest);
    }
}

impl Drop for TempCorpus {
    fn drop(&mut self) {
        if self.root.starts_with(std::env::temp_dir()) {
            fs::remove_dir_all(&self.root).unwrap();
        }
    }
}

fn copy_directory(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_directory(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn canonical_corpus_is_valid_but_not_release_complete() {
    let report = check_corpus(&corpus_root(), CheckMode::Development).unwrap();
    assert_eq!(report.fixtures, 10);
    assert_eq!(report.requirements, 36);
    assert_eq!(report.verified_requirements, 9);
    assert_eq!(report.required_remaining, 26);

    let errors = check_corpus(&corpus_root(), CheckMode::Release).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.contains("ocpp16.authorization is planned"))
    );
}

#[test]
fn changed_schema_checksum_is_rejected() {
    let corpus = TempCorpus::copy();
    let schema = corpus.root.join("schemas/1.6/Heartbeat.json");
    let mut bytes = fs::read(&schema).unwrap();
    bytes.extend_from_slice(b"\n");
    fs::write(schema, bytes).unwrap();

    let errors = check_corpus(&corpus.root, CheckMode::Development).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.contains("checksum changed for schemas/1.6/Heartbeat.json"))
    );
}

#[test]
fn malformed_fixture_fails_even_with_a_rewritten_digest() {
    let corpus = TempCorpus::copy();
    let relative = "wire/2.0.1/boot-notification.json";
    let mut wire = corpus.json(relative);
    wire[3]["chargingStation"]
        .as_object_mut()
        .unwrap()
        .remove("model");
    corpus.write_json(relative, &wire);
    let bytes = fs::read(corpus.root.join(relative)).unwrap();
    corpus.update_wire_digest("wire.ocpp201.boot.valid", &bytes);

    let errors = check_corpus(&corpus.root, CheckMode::Development).unwrap_err();
    assert!(errors.iter().any(|error| {
        error.contains("wire.ocpp201.boot.valid payload fails") && error.contains("model")
    }));
}

#[test]
fn matrix_rejects_duplicates_missing_rows_and_false_completion() {
    let duplicate = TempCorpus::copy();
    let mut inventory = duplicate.json("inventory.json");
    let first = inventory["requirements"][0].clone();
    inventory["requirements"]
        .as_array_mut()
        .unwrap()
        .push(first);
    duplicate.write_json("inventory.json", &inventory);
    let errors = check_corpus(&duplicate.root, CheckMode::Development).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.contains("duplicate requirement ID"))
    );

    let missing = TempCorpus::copy();
    let mut coverage = missing.json("coverage.json");
    coverage["rows"].as_array_mut().unwrap().remove(0);
    missing.write_json("coverage.json", &coverage);
    let errors = check_corpus(&missing.root, CheckMode::Development).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.contains("missing coverage row"))
    );

    let false_completion = TempCorpus::copy();
    let mut coverage = false_completion.json("coverage.json");
    let row = coverage["rows"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|row| row["requirement_id"] == "ocpp16.authorization")
        .unwrap();
    row["status"] = Value::String("verified".to_owned());
    false_completion.write_json("coverage.json", &coverage);
    let errors = check_corpus(&false_completion.root, CheckMode::Development).unwrap_err();
    assert!(errors.iter().any(|error| {
        error.contains("ocpp16.authorization is complete without executable fixture evidence")
    }));
}

#[test]
fn bridge_generated_expected_payloads_are_rejected() {
    let corpus = TempCorpus::copy();
    let mut manifest = corpus.json("fixtures.json");
    manifest["fixtures"][0]["authorship"] = Value::String("bridge_encoder_generated".to_owned());
    corpus.write_json("fixtures.json", &manifest);

    let errors = check_corpus(&corpus.root, CheckMode::Development).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.contains("is not independently authored"))
    );
}
