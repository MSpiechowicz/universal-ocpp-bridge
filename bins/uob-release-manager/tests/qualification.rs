mod qualification {
    pub mod support;
}
use qualification::support::{Fixture, install};
use ring::signature::KeyPair;
use std::fs;
use uob_release_manager::{
    DatabaseContinuity, EvidenceResult, SchemaVersion,
    qualification::{self as gate, Evidence},
    supervisor::{Code, Request},
};

fn check(f: &Fixture, e: &Evidence) -> bool {
    let (bytes, signature) = f.signed(e);
    gate::verify(
        &bytes,
        &signature,
        &f.policy,
        &f.artifact.policy,
        &f.previous,
        &f.artifact.manifest,
        f.now,
    )
    .is_ok()
}

#[test]
fn signed_complete_evidence_requires_full_day_and_exact_inputs() {
    let f = Fixture::new();
    assert!(check(&f, &f.evidence));
    let mutations: Vec<fn(&mut Evidence)> = vec![
        |e| e.format = "bundle-v1".into(),
        |e| e.producer = "another-producer".into(),
        |e| e.candidate_digest = "f".repeat(64),
        |e| e.source_commit = "f".repeat(40),
        |e| e.configuration_schema = SchemaVersion::new(2),
        |e| e.configuration_digest = "f".repeat(64),
        |e| e.dataset_digest = "f".repeat(64),
        |e| e.soak_profile_digest = "f".repeat(64),
        |e| e.soak_started_unix_seconds += 1,
        |e| e.soak_started_unix_seconds = e.soak_finished_unix_seconds + 1,
        |e| {
            e.soak_started_unix_seconds += 1;
            e.soak_finished_unix_seconds += 1;
        },
        |e| {
            e.soak_started_unix_seconds -= 86401;
            e.soak_finished_unix_seconds -= 86401;
        },
        |e| e.soak_passed = false,
        |e| {
            e.suites.pop();
        },
        |e| e.suites[0].passed = false,
        |e| e.suites.push(e.suites[0].clone()),
        |e| e.suites[0].result_digest = "missing".into(),
        |e| e.compatibility.old_binary_read_records = EvidenceResult::Failed,
        |e| e.compatibility.operational_database = DatabaseContinuity::SnapshotRestored,
        |e| {
            e.compatibility.preserved_record_classes.clear();
        },
        |e| e.compatibility.old_artifact = e.compatibility.new_artifact.clone(),
        |e| e.pi_measurements_digest = Some("unmeasured".into()),
    ];
    for (index, mutate) in mutations.iter().enumerate() {
        let mut evidence = f.evidence.clone();
        mutate(&mut evidence);
        assert!(!check(&f, &evidence), "mutation {index}");
    }
}

#[test]
fn forged_modified_oversized_and_unknown_field_documents_are_rejected() {
    let f = Fixture::new();
    let (bytes, signature) = f.signed(&f.evidence);
    let verify = |bytes: &[u8], signature: &[u8]| {
        gate::verify(
            bytes,
            signature,
            &f.policy,
            &f.artifact.policy,
            &f.previous,
            &f.artifact.manifest,
            f.now,
        )
    };
    let forged = f.artifact.key.sign(&bytes);
    assert!(
        verify(&bytes, forged.as_ref()).is_err(),
        "artifact publisher is not evidence authority"
    );
    assert!(verify(&bytes, &[]).is_err());
    let mut altered = bytes.clone();
    altered.push(b' ');
    assert!(verify(&altered, &signature).is_err());
    let oversized = vec![b' '; gate::EVIDENCE_LIMIT + 1];
    assert!(verify(&oversized, f.key.sign(&oversized).as_ref()).is_err());
    let mut unknown = serde_json::to_value(&f.evidence).unwrap();
    unknown["trust_me"] = true.into();
    let unknown = serde_json::to_vec(&unknown).unwrap();
    assert!(verify(&unknown, f.key.sign(&unknown).as_ref()).is_err());
}

#[test]
fn qualification_survives_restart_but_never_activates_or_grants_permission() {
    let f = Fixture::new();
    let request = f.publish();
    let active = fs::read(f.store.join("active")).unwrap();
    let mut manager = f.manager();
    assert_eq!(manager.handle(101, request.clone()).code, Code::Forbidden);
    assert_eq!(manager.handle(100, request).code, Code::Ok);
    let status = manager.handle(100, Request::Status {}).status.unwrap();
    let q = status.qualification.unwrap();
    assert_eq!(q.candidate_digest, f.artifact.digest());
    assert!(q.pi_measurements_digest.is_none());
    assert!(matches!(
        status.last_operation.unwrap().request,
        Request::Qualify { .. }
    ));
    assert_eq!(fs::read(f.store.join("active")).unwrap(), active);
    drop(manager);
    let mut manager = f.manager();
    assert_eq!(
        manager
            .handle(100, Request::Status {})
            .status
            .unwrap()
            .qualification,
        Some(q)
    );
    let promote = Request::Promote {
        digest: f.artifact.digest().into(),
    };
    assert_eq!(manager.handle(102, promote.clone()).code, Code::Forbidden);
    assert_eq!(manager.handle(100, promote).code, Code::PreflightRejected);
    assert_eq!(fs::read(f.store.join("active")).unwrap(), active);
}

#[test]
fn same_release_label_with_changed_bytes_requires_new_qualification() {
    let mut f = Fixture::new();
    let request = f.publish();
    let mut manager = f.manager();
    assert_eq!(manager.handle(100, request.clone()).code, Code::Ok);
    let label = f.artifact.manifest.release_id.clone();
    f.artifact.next_payload();
    install(&f.artifact, &f.store);
    assert_eq!(f.artifact.manifest.release_id, label);
    assert!(
        manager
            .handle(100, Request::Status {})
            .status
            .unwrap()
            .qualification
            .is_none()
    );
    assert_eq!(manager.handle(100, request).code, Code::EvidenceRejected);
    assert_eq!(
        manager
            .handle(
                100,
                Request::Promote {
                    digest: f.artifact.digest().into()
                }
            )
            .code,
        Code::QualificationRequired
    );
}

#[test]
fn policy_change_missing_signature_and_revocation_invalidate_persisted_evidence() {
    let mut f = Fixture::new();
    let request = f.publish();
    let mut manager = f.manager();
    assert_eq!(manager.handle(100, request.clone()).code, Code::Ok);
    drop(manager);
    f.policy.dataset_digest = "f".repeat(64);
    let mut manager = f.manager();
    assert!(
        manager
            .handle(100, Request::Status {})
            .status
            .unwrap()
            .qualification
            .is_none()
    );
    drop(manager);
    f.policy
        .dataset_digest
        .clone_from(&f.evidence.dataset_digest);
    f.policy.authorities[0].ed25519_key = vec![1; 32];
    let mut manager = f.manager();
    assert!(
        manager
            .handle(100, Request::Status {})
            .status
            .unwrap()
            .qualification
            .is_none()
    );
    drop(manager);
    let Request::Qualify {
        evidence_digest, ..
    } = request
    else {
        unreachable!()
    };
    f.policy.authorities[0].ed25519_key = f.key.public_key().as_ref().to_vec();
    assert!(
        f.manager()
            .handle(100, Request::Status {})
            .status
            .unwrap()
            .qualification
            .is_some()
    );
    fs::remove_file(
        f.state
            .join("evidence")
            .join(format!("{evidence_digest}.sig")),
    )
    .unwrap();
    assert!(
        f.manager()
            .handle(100, Request::Status {})
            .status
            .unwrap()
            .qualification
            .is_none()
    );
}

#[test]
fn paths_unsigned_inline_claims_and_empty_policy_cannot_qualify() {
    let mut f = Fixture::new();
    let mut manager = f.manager();
    assert_eq!(
        manager
            .handle(
                100,
                Request::Qualify {
                    digest: f.artifact.digest().into(),
                    evidence_digest: "../../etc/passwd".into(),
                }
            )
            .code,
        Code::InvalidRequest
    );
    assert!(
        serde_json::from_str::<Request>(
            r#"{"operation":"qualify","digest":"a","evidence_digest":"b","passed":true}"#
        )
        .is_err()
    );
    f.policy.required_suites.clear();
    assert!(f.policy.validate().is_err());
    f.policy = Fixture::new().policy;
    f.policy.authorities.clear();
    assert!(f.policy.validate().is_err());
}

#[test]
fn evidence_inbox_rejects_links_and_content_address_mismatch() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let request = f.publish();
    let Request::Qualify {
        evidence_digest, ..
    } = &request
    else {
        unreachable!()
    };
    let path = f
        .state
        .join("evidence")
        .join(format!("{evidence_digest}.json"));
    let original = fs::read(&path).unwrap();
    let mut manager = f.manager();
    fs::write(&path, b"{}").unwrap();
    assert_eq!(
        manager.handle(100, request.clone()).code,
        Code::EvidenceRejected
    );
    fs::remove_file(&path).unwrap();
    let outside = f.state.join("outside.json");
    fs::write(&outside, &original).unwrap();
    symlink(&outside, &path).unwrap();
    assert_eq!(
        manager.handle(100, request.clone()).code,
        Code::EvidenceRejected
    );
    fs::remove_file(&path).unwrap();
    fs::hard_link(&outside, &path).unwrap();
    assert_eq!(manager.handle(100, request).code, Code::EvidenceRejected);
    assert_eq!(fs::read(&outside).unwrap(), original);
}

#[test]
fn evidence_age_boundary_and_policy_revocation_are_rechecked() {
    let mut f = Fixture::new();
    let (bytes, signature) = f.signed(&f.evidence);
    let verify = |f: &Fixture, now| {
        gate::verify(
            &bytes,
            &signature,
            &f.policy,
            &f.artifact.policy,
            &f.previous,
            &f.artifact.manifest,
            now,
        )
    };
    assert!(verify(&f, f.now + f.policy.maximum_evidence_age_seconds).is_ok());
    assert!(verify(&f, f.now + f.policy.maximum_evidence_age_seconds + 1).is_err());
    f.artifact
        .policy
        .security
        .revoked_artifacts
        .insert(f.previous.compatibility.artifact_digest.clone());
    assert!(verify(&f, f.now).is_err());
}
