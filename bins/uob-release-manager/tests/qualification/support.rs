#[allow(dead_code)]
#[path = "../artifact_store/support.rs"]
pub mod artifacts;
use ring::signature::{Ed25519KeyPair, KeyPair};
use std::{collections::BTreeSet, fs, os::unix::fs::PermissionsExt, path::PathBuf};
use uob_release_manager::{
    ConfigurationProjectionEvidence, DatabaseContinuity, DurableRecordClass, EvidenceResult,
    RollbackCycleEvidence,
    artifacts::{ArtifactStore, BundleManifest},
    qualification::{self, Authority, Evidence, Policy, Suite},
    supervisor::{Grant, Permission, Request, Supervisor},
};

pub struct Fixture {
    pub artifact: artifacts::Fixture,
    pub previous: BundleManifest,
    pub key: Ed25519KeyPair,
    pub policy: Policy,
    pub evidence: Evidence,
    pub store: PathBuf,
    pub state: PathBuf,
    pub now: u64,
}

impl Fixture {
    pub fn new() -> Self {
        Self::with_candidate(None)
    }

    pub fn with_candidate(binary: Option<&[u8]>) -> Self {
        let mut artifact = artifacts::Fixture::new();
        configure_schema(&mut artifact, binary.is_some());
        let store = artifact.root.join("store");
        let state = artifact.root.join("state");
        for path in [&store, &state, &state.join("evidence")] {
            fs::create_dir(path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        install(&artifact, &store);
        let previous = artifact.manifest.clone();
        // Explicit fixture bootstrap of an independently designated previous-good artifact.
        fs::write(store.join("active"), artifact.digest()).unwrap();
        fs::write(store.join("previous-good"), artifact.digest()).unwrap();
        artifact.next_payload();
        configure_payload(&mut artifact, binary);
        artifact.policy.backup_reserve_bytes = 4 * 1024 * 1024;
        install(&artifact, &store);
        let key = Ed25519KeyPair::from_seed_unchecked(&[42; 32]).unwrap();
        let policy = Policy {
            authorities: vec![Authority {
                producer: "trusted-staging-harness".into(),
                ed25519_key: key.public_key().as_ref().to_vec(),
            }],
            configuration_digest: "a".repeat(64),
            dataset_digest: "b".repeat(64),
            soak_profile_digest: "c".repeat(64),
            required_suites: BTreeSet::from([
                "acceptance-ocpp16".into(),
                "acceptance-ocpp201".into(),
            ]),
            maximum_evidence_age_seconds: 86400,
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let evidence = Evidence {
            format: "uob-qualification-v1".into(),
            producer: policy.authorities[0].producer.clone(),
            candidate_digest: artifact.digest().into(),
            source_commit: artifact.manifest.source_commit.clone(),
            configuration_schema: artifact.policy.current_formats.configuration,
            configuration_digest: policy.configuration_digest.clone(),
            dataset_digest: policy.dataset_digest.clone(),
            soak_profile_digest: policy.soak_profile_digest.clone(),
            soak_started_unix_seconds: now - 86400,
            soak_finished_unix_seconds: now,
            soak_passed: true,
            suites: policy
                .required_suites
                .iter()
                .map(|id| Suite {
                    id: id.clone(),
                    passed: true,
                    result_digest: "d".repeat(64),
                })
                .collect(),
            compatibility: RollbackCycleEvidence {
                old_artifact: previous.compatibility.artifact_digest.clone(),
                new_artifact: artifact.manifest.compatibility.artifact_digest.clone(),
                returned_artifact: previous.compatibility.artifact_digest.clone(),
                resulting_versions: artifact.policy.current_formats,
                preserved_record_classes: BTreeSet::from([
                    DurableRecordClass::Transaction,
                    DurableRecordClass::MeterData,
                    DurableRecordClass::CommandId,
                    DurableRecordClass::TargetDelivery,
                    DurableRecordClass::ExportCheckpoint,
                    DurableRecordClass::AuditRecord,
                ]),
                candidate_created_records: EvidenceResult::Passed,
                old_binary_read_records: EvidenceResult::Passed,
                old_binary_wrote_records: EvidenceResult::Passed,
                unknown_values_round_trip: EvidenceResult::Passed,
                operational_database: DatabaseContinuity::ReusedInPlace,
                external_database: DatabaseContinuity::ReusedInPlace,
                configuration_projection: ConfigurationProjectionEvidence {
                    source_version: artifact.policy.current_formats.configuration,
                    target_version: artifact.policy.current_formats.configuration,
                    preserves_unknown_fields: true,
                    mutates_durable_state: false,
                },
            },
            pi_measurements_digest: None,
        };
        Self {
            artifact,
            previous,
            key,
            policy,
            evidence,
            store,
            state,
            now,
        }
    }

    pub fn signed(&self, evidence: &Evidence) -> (Vec<u8>, Vec<u8>) {
        let bytes = serde_json::to_vec(evidence).unwrap();
        let signature = self.key.sign(&bytes).as_ref().to_vec();
        (bytes, signature)
    }

    pub fn publish(&self) -> Request {
        let (bytes, signature) = self.signed(&self.evidence);
        let reference = qualification::digest(&bytes);
        fs::write(
            self.state
                .join("evidence")
                .join(format!("{reference}.json")),
            bytes,
        )
        .unwrap();
        fs::write(
            self.state.join("evidence").join(format!("{reference}.sig")),
            signature,
        )
        .unwrap();
        Request::Qualify {
            digest: self.artifact.digest().into(),
            evidence_digest: reference,
        }
    }

    pub fn manager(&self) -> Supervisor {
        Supervisor::open(
            &self.state,
            &self.store,
            self.artifact.policy.clone(),
            vec![
                Grant {
                    uid: 100,
                    permissions: vec![Permission::Read, Permission::Stage, Permission::Activate],
                },
                Grant {
                    uid: 101,
                    permissions: vec![Permission::Read],
                },
                Grant {
                    uid: 102,
                    permissions: vec![Permission::Stage],
                },
            ],
        )
        .unwrap()
        .with_qualification_policy(self.policy.clone())
        .unwrap()
    }
}

pub fn install(f: &artifacts::Fixture, store: &std::path::Path) {
    let (bytes, signature) = f.signed();
    ArtifactStore::open(store)
        .unwrap()
        .install(&bytes, &signature, &mut f.payload.as_slice(), &f.policy)
        .unwrap();
}

fn configure_schema(artifact: &mut artifacts::Fixture, preflight: bool) {
    if preflight {
        let version = uob_release_manager::SchemaVersion::new(5);
        let range = uob_release_manager::SchemaRange::new(version, version).unwrap();
        artifact.manifest.compatibility.formats.operational_sqlite =
            uob_release_manager::FormatSupport {
                readable: range,
                writable: range,
            };
        artifact.policy.current_formats.operational_sqlite = version;
    }
}

fn configure_payload(artifact: &mut artifacts::Fixture, binary: Option<&[u8]>) {
    if let Some(binary) = binary {
        artifact.payload = binary.to_vec();
        artifact.manifest.files = vec![uob_release_manager::artifacts::BundleFile {
            path: "bin/uob".into(),
            bytes: binary.len() as u64,
        }];
        artifact.manifest.compatibility.artifact_digest =
            uob_contracts::ArtifactDigest::new(qualification::digest(binary)).unwrap();
    }
}
