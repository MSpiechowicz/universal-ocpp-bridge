use uob_application::{
    AccessPermission, BridgeTargetFactory, ConfigurationError, ConfigurationErrorCode,
    ConfigurationValue, CredentialReference, TargetConfiguration,
};
use uob_contracts::{AuthenticatedCommandOrigin, Environment, TargetInstanceId};

use super::{
    EMS_SCADA_HTTP_TARGET_KIND, EmsScadaHttpTargetFactory, credentials::resolve_credentials,
};

fn target_id() -> TargetInstanceId {
    TargetInstanceId::new("main").expect("target instance")
}

fn configuration(listen_addr: &str) -> TargetConfiguration {
    TargetConfiguration::new(target_id(), 1).with_setting(
        "listen_addr".to_owned(),
        ConfigurationValue::Text(listen_addr.to_owned()),
    )
}

fn credential(path: &str) -> ConfigurationValue {
    ConfigurationValue::CredentialReference(
        CredentialReference::new(path).expect("credential reference"),
    )
}

fn validate(
    environment: Environment,
    configuration: &TargetConfiguration,
) -> Result<(), ConfigurationError> {
    <EmsScadaHttpTargetFactory as BridgeTargetFactory<(), ()>>::validate(
        &EmsScadaHttpTargetFactory::new(environment),
        configuration,
    )
    .map(|_| ())
}

#[test]
fn the_factory_advertises_its_stable_kind() {
    let factory = EmsScadaHttpTargetFactory::new(Environment::Demo);
    assert_eq!(
        <EmsScadaHttpTargetFactory as BridgeTargetFactory<(), ()>>::kind(&factory),
        EMS_SCADA_HTTP_TARGET_KIND
    );
    assert_eq!(EMS_SCADA_HTTP_TARGET_KIND, "ems-scada.http");
}

#[test]
fn a_loopback_listener_needs_no_remote_enablement() {
    assert!(validate(Environment::Demo, &configuration("127.0.0.1:9080")).is_ok());
}

#[test]
fn an_unparsable_listen_address_is_rejected_by_field() {
    let error = validate(Environment::Demo, &configuration("not-an-address"))
        .expect_err("invalid listen address");
    assert_eq!(error.code(), ConfigurationErrorCode::InvalidField);
}

#[test]
fn production_requires_referenced_integration_credentials() {
    let error = validate(Environment::Production, &configuration("127.0.0.1:9080"))
        .expect_err("missing production credentials");
    assert_eq!(error.code(), ConfigurationErrorCode::MissingField);

    let authenticated = configuration("127.0.0.1:9080").with_setting(
        "credentials_file".to_owned(),
        credential("/run/uob/ems.toml"),
    );
    assert!(validate(Environment::Production, &authenticated).is_ok());
}

#[test]
fn a_public_listener_requires_enablement_tls_and_credentials() {
    let exposed = configuration("0.0.0.0:9080");
    assert_eq!(
        validate(Environment::Demo, &exposed)
            .expect_err("implicit exposure")
            .code(),
        ConfigurationErrorCode::MissingField
    );

    let enabled = exposed.with_setting(
        "remote_access_enabled".to_owned(),
        ConfigurationValue::Boolean(true),
    );
    assert_eq!(
        validate(Environment::Demo, &enabled)
            .expect_err("missing TLS identity")
            .code(),
        ConfigurationErrorCode::MissingField
    );

    let with_tls = enabled
        .with_setting(
            "tls_certificate_file".to_owned(),
            credential("/run/uob/ems.crt"),
        )
        .with_setting(
            "tls_private_key_file".to_owned(),
            credential("/run/uob/ems.key"),
        );
    assert_eq!(
        validate(Environment::Demo, &with_tls)
            .expect_err("missing scoped credentials")
            .code(),
        ConfigurationErrorCode::MissingField
    );

    let complete = with_tls.with_setting(
        "credentials_file".to_owned(),
        credential("/run/uob/ems.toml"),
    );
    assert!(validate(Environment::Demo, &complete).is_ok());
}

#[test]
fn half_a_tls_identity_is_rejected_rather_than_silently_ignored() {
    let certificate_only = configuration("127.0.0.1:9080").with_setting(
        "tls_certificate_file".to_owned(),
        credential("/run/uob/ems.crt"),
    );
    assert_eq!(
        validate(Environment::Demo, &certificate_only)
            .expect_err("unpaired TLS material")
            .code(),
        ConfigurationErrorCode::MissingField
    );
}

#[test]
fn absent_credentials_resolve_to_an_open_local_listener() {
    let credentials = resolve_credentials(None, &target_id()).expect("no credential file");
    assert!(credentials.is_empty());
}

#[test]
fn every_integration_grant_carries_a_target_origin_and_never_a_management_one() {
    let file = "[[principals]]\nid = 'ems-reader'\ntoken = 'reader-token'\npermissions = ['read']\nbridges = ['site-01']\n";
    let credentials = credentials_from(file).expect("valid credential file");

    let principal = credentials
        .authenticate("reader-token")
        .expect("configured token");
    assert_eq!(principal.permissions(), [AccessPermission::Read]);
    assert!(matches!(
        principal.grant().origin(),
        AuthenticatedCommandOrigin::Target { .. }
    ));
    assert!(credentials.authenticate("reader-token-x").is_none());
    assert!(credentials.authenticate("").is_none());
}

#[test]
fn a_principal_without_a_resource_scope_is_rejected() {
    let file =
        "[[principals]]\nid = 'ems-reader'\ntoken = 'reader-token'\npermissions = ['read']\n";
    assert_eq!(
        credentials_from(file).err(),
        Some("ems_scada_http.credentials_invalid")
    );
}

#[test]
fn duplicate_principals_and_shared_tokens_are_rejected() {
    let duplicate_id = "[[principals]]\nid = 'a'\ntoken = 'one'\npermissions = ['read']\nbridges = ['site-01']\n\n[[principals]]\nid = 'a'\ntoken = 'two'\npermissions = ['read']\nbridges = ['site-01']\n";
    let shared_token = "[[principals]]\nid = 'a'\ntoken = 'one'\npermissions = ['read']\nbridges = ['site-01']\n\n[[principals]]\nid = 'b'\ntoken = 'one'\npermissions = ['read']\nbridges = ['site-01']\n";
    assert_eq!(
        credentials_from(duplicate_id).err(),
        Some("ems_scada_http.credentials_invalid")
    );
    assert_eq!(
        credentials_from(shared_token).err(),
        Some("ems_scada_http.credentials_invalid")
    );
}

#[test]
fn a_diagnostic_or_administration_permission_cannot_be_named() {
    let file = "[[principals]]\nid = 'a'\ntoken = 'one'\npermissions = ['debug_capture']\nbridges = ['site-01']\n";
    assert_eq!(
        credentials_from(file).err(),
        Some("ems_scada_http.credentials_invalid")
    );
}

/// Writes one private credential file and resolves it through the real bounded reader.
fn credentials_from(document: &str) -> Result<super::IntegrationCredentials, &'static str> {
    let directory = crate::configuration::tests::TestDirectory::new();
    let path = directory.path("integration.toml");
    std::fs::write(&path, document).expect("write credential file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("restrict credential file");
    }
    resolve_credentials(
        Some(&CredentialReference::new(path.to_str().expect("path text")).expect("reference")),
        &target_id(),
    )
}

struct TestDirectory(std::path::PathBuf);

impl TestDirectory {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "uob-ems-http-credentials-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).expect("create test directory");
        Self(path)
    }

    fn path(&self, name: &str) -> std::path::PathBuf {
        self.0.join(name)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).expect("remove test directory");
    }
}
