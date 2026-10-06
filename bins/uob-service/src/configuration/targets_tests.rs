use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use serde_json::json;
use uob_application::{
    DeliveryDispositionAction, DeliveryDispositionRecord, DeliveryDispositionState,
    TargetBacklogFact, TargetDeliveryDestination,
};
use uob_contracts::{BridgeId, Environment, PrincipalId, TargetInstanceId, UtcTimestamp};
use uob_management_adapter::{SettingView, TargetConfigurationCandidate};

use super::{DurableFacts, Evaluation, Sections, catalog_view, evaluate};
use crate::configuration::{ConfigurationLoadError, load};

struct Fixture(PathBuf);

impl Fixture {
    fn new(environment: &str, targets: &str) -> Self {
        let root = std::env::temp_dir().join(format!("uob-targets-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let fixture = Self(root);
        fixture.write_base(environment, targets);
        fixture
    }

    fn write_base(&self, environment: &str, targets: &str) {
        std::fs::write(
            self.config(),
            format!(
                "[bridge]\nid = 'site-01'\nenvironment = '{environment}'\ntarget_id = 'main'\n\n\
                 [configuration_api]\nstaged_targets_file = '{}'\n\n{targets}",
                self.staged().display()
            ),
        )
        .unwrap();
    }

    fn config(&self) -> PathBuf {
        self.0.join("bridge.toml")
    }

    fn staged(&self) -> PathBuf {
        self.0.join("staged-targets.toml")
    }

    fn sections(&self) -> Sections {
        Sections::read(&self.config()).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const MQTT_MAIN: &str = "[[targets]]\nid = 'main'\nkind = 'mqtt'\nenabled = true\n\n\
    [targets.settings]\nbroker_url = 'mqtt://127.0.0.1:1883'\nallow_plaintext = true\n";

fn mqtt(enabled: bool, profile: Option<&str>) -> serde_json::Value {
    let mut settings = json!({"broker_url": "mqtt://127.0.0.1:1883", "allow_plaintext": true});
    if let Some(profile) = profile {
        settings["profile"] = json!(profile);
    }
    json!({"id": "main", "kind": "mqtt", "enabled": enabled, "settings": settings})
}

fn http(listen_addr: &str) -> serde_json::Value {
    json!({"id": "api", "kind": "ems-scada.http", "enabled": true,
           "settings": {"listen_addr": listen_addr}})
}

fn candidate(target_id: &str, targets: &[serde_json::Value]) -> TargetConfigurationCandidate {
    serde_json::from_value(json!({"target_id": target_id, "targets": targets})).unwrap()
}

fn destination(target: &str, revision: u64) -> TargetDeliveryDestination {
    TargetDeliveryDestination {
        target_instance_id: TargetInstanceId::new(target).unwrap(),
        configuration_revision: revision,
    }
}

fn backlog(target: &str, revision: u64, critical: u64) -> TargetBacklogFact {
    TargetBacklogFact {
        destination: destination(target, revision),
        pending_critical_deliveries: critical,
        pending_deliveries: critical + 1,
    }
}

fn disposition(target: &str, revision: u64) -> DeliveryDispositionRecord {
    DeliveryDispositionRecord {
        audit_event_id: format!("disposition-{target}-{revision}"),
        destination: destination(target, revision),
        action: DeliveryDispositionAction::Archive,
        principal_id: PrincipalId::new("operator-a").unwrap(),
        authorized_at: UtcTimestamp::new(time::OffsetDateTime::UNIX_EPOCH),
        state: DeliveryDispositionState::Authorized,
        settled_at: None,
        critical_deliveries: None,
        deliveries: None,
    }
}

fn run(
    fixture: &Fixture,
    candidate: &TargetConfigurationCandidate,
    running: Option<&TargetDeliveryDestination>,
    backlog: &[TargetBacklogFact],
    dispositions: &[DeliveryDispositionRecord],
) -> Evaluation {
    evaluate(
        fixture.sections(),
        candidate,
        &DurableFacts {
            running,
            backlog,
            dispositions,
        },
    )
}

fn codes(evaluation: &Evaluation) -> Vec<&str> {
    evaluation
        .report
        .errors
        .iter()
        .map(|issue| issue.code.as_str())
        .collect()
}

fn persist(fixture: &Fixture, evaluation: &Evaluation) -> String {
    evaluation
        .persist(&fixture.staged(), "operator-a", "2026-10-06T09:00:00Z")
        .unwrap()
        .unwrap()
}

#[test]
fn catalog_lists_registry_kinds_and_mqtt_presets_without_a_second_target() {
    let catalog = catalog_view(&BridgeId::new("site-01").unwrap(), Environment::Demo).unwrap();
    let kinds = catalog
        .kinds
        .iter()
        .map(|kind| (kind.kind.as_str(), kind.available))
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        [
            ("ems-scada.http", true),
            ("ems-scada.opcua", false),
            ("mqtt", true)
        ]
    );
    let mqtt = &catalog.kinds[2];
    assert_eq!(
        mqtt.presets
            .iter()
            .map(|preset| preset.id.as_str())
            .collect::<Vec<_>>(),
        ["standard", "ems-scada"]
    );
    assert!(
        mqtt.configuration_schema.iter().any(|field| {
            field.name == "credentials_file" && field.kind == "credential_reference"
        })
    );
    assert_eq!(catalog.kinds[0].display_family.id, "ems-scada");
}

#[test]
fn unavailable_and_unknown_kinds_are_rejected() {
    let fixture = Fixture::new("demo", MQTT_MAIN);
    let opcua = json!({"id": "opc", "kind": "ems-scada.opcua", "enabled": true});
    let evaluation = run(&fixture, &candidate("opc", &[opcua]), None, &[], &[]);
    assert_eq!(codes(&evaluation), ["target.unavailable_kind"]);
    assert_eq!(
        evaluation.report.errors[0].target_id.as_deref(),
        Some("opc")
    );
    let unknown = json!({"id": "x", "kind": "vendor.custom", "enabled": true});
    let evaluation = run(&fixture, &candidate("x", &[unknown]), None, &[], &[]);
    assert_eq!(codes(&evaluation), ["target.unknown_kind"]);
}

#[test]
fn http_target_validates_without_mqtt_and_requires_restart() {
    let fixture = Fixture::new("demo", MQTT_MAIN);
    let running = destination("main", 1);
    let evaluation = run(
        &fixture,
        &candidate("api", &[mqtt(false, None), http("127.0.0.1:9080")]),
        Some(&running),
        &[],
        &[],
    );
    assert!(evaluation.report.valid, "{:?}", codes(&evaluation));
    assert_eq!(
        evaluation
            .report
            .next_destination
            .as_ref()
            .unwrap()
            .target_id,
        "api"
    );
    assert!(evaluation.report.restart_required);
    // The disabled MQTT entry is unchanged and keeps its revision.
    assert_eq!(evaluation.report.assigned_revisions["main"], 1);
    assert_eq!(evaluation.report.assigned_revisions["api"], 1);

    persist(&fixture, &evaluation);
    let loaded = load(&fixture.config()).unwrap();
    let selection = loaded.service.target_selection.unwrap();
    assert_eq!(selection.target_id.as_str(), "api");
    assert_eq!(selection.catalog.kind.as_str(), "ems-scada.http");
}

#[test]
fn ems_scada_mqtt_profile_is_the_same_mqtt_kind() {
    let fixture = Fixture::new("demo", MQTT_MAIN);
    let running = destination("main", 1);
    let evaluation = run(
        &fixture,
        &candidate("main", &[mqtt(true, Some("ems-scada"))]),
        Some(&running),
        &[],
        &[],
    );
    assert!(evaluation.report.valid, "{:?}", codes(&evaluation));
    // Changed settings bind new work to a new revision of the same instance.
    assert_eq!(evaluation.report.assigned_revisions["main"], 2);
    assert!(evaluation.report.restart_required);
    persist(&fixture, &evaluation);
    let view = fixture.sections().view();
    assert_eq!(view.source, "staged");
    assert_eq!(view.targets.len(), 1);
    assert_eq!(view.targets[0].kind, "mqtt");
    assert_eq!(view.targets[0].preset.as_deref(), Some("ems-scada"));
}

#[test]
fn unsafe_listener_and_credential_settings_fail_with_sanitized_codes() {
    let fixture = Fixture::new("demo", MQTT_MAIN);
    let evaluation = run(
        &fixture,
        &candidate("api", &[http("0.0.0.0:9080")]),
        None,
        &[],
        &[],
    );
    assert_eq!(codes(&evaluation), ["target.missing_field"]);
    assert_eq!(
        evaluation.report.errors[0].field.as_deref(),
        Some("remote_access_enabled")
    );

    let secret = "hunter2-secret";
    let mut leaky = mqtt(true, None);
    leaky["settings"]["broker_url"] = json!(format!("mqtts://operator:{secret}@broker:8883"));
    let evaluation = run(&fixture, &candidate("main", &[leaky]), None, &[], &[]);
    assert!(!evaluation.report.valid);
    let encoded = serde_json::to_string(&evaluation.report).unwrap();
    assert!(!encoded.contains(secret), "{encoded}");

    let production = Fixture::new("production", "");
    let evaluation = run(
        &production,
        &candidate("api", &[http("127.0.0.1:9080")]),
        None,
        &[],
        &[],
    );
    assert_eq!(codes(&evaluation), ["target.missing_field"]);
    assert_eq!(
        evaluation.report.errors[0].field.as_deref(),
        Some("credentials_file")
    );
}

#[test]
fn pending_critical_work_blocks_a_change_until_an_exact_disposition_covers_it() {
    let fixture = Fixture::new("demo", MQTT_MAIN);
    let running = destination("main", 1);
    let change = candidate("api", &[mqtt(false, None), http("127.0.0.1:9080")]);
    let pending = [backlog("main", 1, 3), backlog("main", 7, 0)];

    let blocked = run(&fixture, &change, Some(&running), &pending, &[]);
    assert_eq!(codes(&blocked), ["target.pending_destination_change"]);
    assert_eq!(blocked.report.blocking_destinations.len(), 1);
    assert_eq!(
        blocked.report.blocking_destinations[0].pending_critical_deliveries,
        3
    );
    assert_eq!(blocked.report.pending_critical_deliveries, 3);
    assert!(
        blocked
            .persist(&fixture.staged(), "operator-a", "now")
            .unwrap()
            .is_none(),
        "an invalid candidate is never persisted"
    );
    assert!(!fixture.staged().exists());

    let wrong_revision = run(
        &fixture,
        &change,
        Some(&running),
        &pending,
        &[disposition("main", 2)],
    );
    assert_eq!(
        codes(&wrong_revision),
        ["target.pending_destination_change"]
    );

    let covered = run(
        &fixture,
        &change,
        Some(&running),
        &pending,
        &[disposition("main", 1)],
    );
    assert!(covered.report.valid, "{:?}", codes(&covered));
    assert_eq!(covered.report.dispositions, ["disposition-main-1"]);

    // Keeping the destination never needs a disposition: its own work still drains.
    let unchanged = run(
        &fixture,
        &candidate("main", &[mqtt(true, None)]),
        Some(&running),
        &pending,
        &[],
    );
    assert!(unchanged.report.valid, "{:?}", codes(&unchanged));
    assert!(!unchanged.report.restart_required);
}

#[test]
fn a_changed_instance_never_reuses_a_revision_that_owns_old_work() {
    let fixture = Fixture::new("demo", MQTT_MAIN);
    let mut changed = mqtt(true, None);
    changed["settings"]["broker_url"] = json!("mqtt://127.0.0.1:1884");
    let evaluation = run(
        &fixture,
        &candidate("main", &[changed]),
        Some(&destination("main", 1)),
        &[backlog("main", 4, 0)],
        &[],
    );
    assert_eq!(evaluation.report.assigned_revisions["main"], 5);
}

#[test]
fn staged_section_is_private_and_conflicting_startup_edits_fail_closed() {
    let fixture = Fixture::new("demo", MQTT_MAIN);
    let evaluation = run(
        &fixture,
        &candidate("api", &[mqtt(false, None), http("127.0.0.1:9080")]),
        None,
        &[],
        &[],
    );
    // A temporary file left by a crash mid-apply never blocks later applies.
    let stale = fixture
        .0
        .join(format!(".staged-targets.toml.{}.tmp", std::process::id()));
    std::fs::write(&stale, "partial").unwrap();
    let digest = persist(&fixture, &evaluation);
    assert!(!stale.exists());
    assert_eq!(fixture.sections().effective().digest(), digest);
    let mode = std::fs::metadata(fixture.staged())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o077, 0);

    // Unrelated startup-file edits keep the staged section.
    std::fs::write(
        fixture.config(),
        format!(
            "{}\n[lifecycle]\nshutdown_timeout_seconds = 30\n",
            std::fs::read_to_string(fixture.config()).unwrap()
        ),
    )
    .unwrap();
    assert!(load(&fixture.config()).is_ok());

    // An edited startup target section conflicts instead of silently winning or losing.
    fixture.write_base("demo", &MQTT_MAIN.replace("1883", "1885"));
    assert!(matches!(
        load(&fixture.config()),
        Err(ConfigurationLoadError::StagedTargetsConflict)
    ));
}

#[test]
fn applying_the_startup_section_again_removes_the_overlay() {
    let fixture = Fixture::new("demo", MQTT_MAIN);
    let staged = run(
        &fixture,
        &candidate("api", &[mqtt(false, None), http("127.0.0.1:9080")]),
        None,
        &[],
        &[],
    );
    persist(&fixture, &staged);
    assert!(fixture.staged().exists());
    let revert = run(
        &fixture,
        &candidate("main", &[mqtt(true, None)]),
        None,
        &[],
        &[],
    );
    let digest = persist(&fixture, &revert);
    assert!(!fixture.staged().exists());
    assert_eq!(fixture.sections().view().source, "base");
    assert_eq!(fixture.sections().effective().digest(), digest);
}

#[test]
fn view_shows_credentials_only_as_references_and_withholds_risky_text() {
    let fixture = Fixture::new(
        "demo",
        &format!(
            "{MQTT_MAIN}credentials_file = '/etc/uob/secrets/mqtt.toml'\n\n\
             [[targets]]\nid = 'old'\nkind = 'mqtt'\n\n[targets.settings]\n\
             broker_url = 'mqtts://user:s3cr3t@broker:8883'\npassword = 'p4ssw0rd'\n"
        ),
    );
    let view = fixture.sections().view();
    let main = &view.targets[0].settings;
    assert_eq!(
        main["credentials_file"],
        SettingView::CredentialReference {
            credential_reference: "/etc/uob/secrets/mqtt.toml".to_owned()
        }
    );
    assert_eq!(
        main["broker_url"],
        SettingView::Text("mqtt://127.0.0.1:1883".to_owned())
    );
    let old = &view.targets[1].settings;
    assert_eq!(old["broker_url"], SettingView::Redacted { redacted: true });
    assert_eq!(old["password"], SettingView::Redacted { redacted: true });
    let encoded = serde_json::to_string(&view).unwrap();
    assert!(!encoded.contains("s3cr3t") && !encoded.contains("p4ssw0rd"));
}

#[test]
fn startup_file_without_a_staged_path_cannot_apply() {
    let fixture = Fixture::new("demo", MQTT_MAIN);
    let document = std::fs::read_to_string(fixture.config()).unwrap();
    let start = document.find("[configuration_api]").unwrap();
    let end = document.find("[[targets]]").unwrap();
    std::fs::write(
        fixture.config(),
        format!("{}{}", &document[..start], &document[end..]),
    )
    .unwrap();
    let evaluation = run(
        &fixture,
        &candidate("main", &[mqtt(true, None)]),
        None,
        &[],
        &[],
    );
    assert!(evaluation.staged_path().is_none());
    assert!(Path::new(&fixture.config()).exists());
}
