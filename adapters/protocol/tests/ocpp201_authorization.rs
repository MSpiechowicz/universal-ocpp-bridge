#[path = "ocpp201_authorization/edges.rs"]
mod edges;
#[path = "endpoint_support/mod.rs"]
mod endpoint_support;
#[path = "ocpp201_authorization/wire.rs"]
mod wire;
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};
use uob_application::charging_identity::*;
use uob_application::{
    AuthorizationProviderDescriptor, AuthorizationProviderError, ChargerObservation, CommandClock,
    LocalAuthorizationService,
};
use uob_contracts::{ResourceRef, UtcTimestamp};
use uob_protocol_adapter::{DecodeErrorKind, v201};
use uob_provider_adapter::LocalChargingIdentityProvider;
#[path = "ocpp201_authorization/support.rs"]
mod auth_support;
use auth_support::*;
use uob_application::AuthorizationState;
const AUTHORIZE: &[u8] =
    include_bytes!("../../../tests/ocpp-fixtures/corpus/wire/2.0.1/authorize.json");
const CERTIFICATE: &[u8] =
    include_bytes!("../../../tests/ocpp-fixtures/corpus/wire/2.0.1/authorize-certificate.json");

#[tokio::test]
async fn durable_policy_maps_status_expiry_scope_and_recovery() {
    let db = Database::new();
    let service = db.recover().await;
    assert_eq!(
        response(&service, AUTHORIZE, 0).await[2]["idTokenInfo"]["status"],
        "Unknown"
    );
    let reference = reference(AUTHORIZE).await;
    allow(
        &service,
        resource(),
        reference.clone(),
        AuthorizationState::Active,
        1,
        Some(timestamp(5)),
    )
    .await;
    assert_eq!(
        response(&service, AUTHORIZE, 4).await,
        serde_json::from_slice::<Value>(include_bytes!(
            "../../../tests/ocpp-fixtures/corpus/wire/2.0.1/authorize-accepted.json"
        ))
        .unwrap()
    );
    assert_eq!(
        response(&service, AUTHORIZE, 5).await[2]["idTokenInfo"]["status"],
        "Expired"
    );
    let mut other = resource();
    other.station_id = uob_contracts::StationId::new("other").unwrap();
    let denied = v201::authorize_call(
        AUTHORIZE,
        &other,
        &service,
        &LocalChargingIdentityProvider,
        &Clock(AtomicU8::new(4)),
        Duration::from_secs(1),
    )
    .await
    .unwrap();
    assert_eq!(denied[2]["idTokenInfo"]["status"], "NotAtThisLocation");
    allow(
        &service,
        resource(),
        reference,
        AuthorizationState::Revoked,
        2,
        None,
    )
    .await;
    drop(service);
    let recovered = db.recover().await;
    assert_eq!(
        response(&recovered, AUTHORIZE, 6).await,
        serde_json::from_slice::<Value>(include_bytes!(
            "../../../tests/ocpp-fixtures/corpus/wire/2.0.1/authorize-blocked.json"
        ))
        .unwrap()
    );
}

#[tokio::test]
async fn typed_local_tokens_are_case_insensitive_without_cross_type_aliasing() {
    let mut frame: Value = serde_json::from_slice(AUTHORIZE).unwrap();
    frame[3]["idToken"]["idToken"] = json!("local-user-1");
    assert_eq!(
        reference(AUTHORIZE).await,
        reference(frame.to_string().as_bytes()).await
    );
    frame[3]["idToken"]["type"] = json!("Central");
    assert_ne!(
        reference(AUTHORIZE).await,
        reference(frame.to_string().as_bytes()).await
    );
    let db = Database::new();
    let service = db.recover().await;
    for kind in ["eMAID", "NoAuthorization"] {
        frame[3]["idToken"]["type"] = json!(kind);
        assert_eq!(
            response(&service, frame.to_string().as_bytes(), 0).await[2]["idTokenInfo"]["status"],
            "Invalid"
        );
    }
    assert_eq!(
        response(&service, CERTIFICATE, 0).await[2]["idTokenInfo"]["status"],
        "Invalid"
    );
}

struct Provider {
    resolution: Result<ChargingIdentityResolution, AuthorizationProviderError>,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    expected_certificate: bool,
}
impl ChargingIdentityProvider for Provider {
    fn descriptor(&self) -> AuthorizationProviderDescriptor {
        AuthorizationProviderDescriptor {
            kind: "test.typed",
            test_only: true,
        }
    }
    fn resolve<'a>(
        &'a self,
        identity: &'a PresentedChargingIdentity,
    ) -> ChargingIdentityFuture<'a> {
        Box::pin(async move {
            if self.expected_certificate {
                assert_eq!(identity.kind, ChargingTokenKind::Emaid);
                assert_eq!(identity.token, "DE-TEST-C12345678");
                assert_eq!(identity.additional[0].token, "ALTERNATE-TEST");
                assert_eq!(identity.additional[0].kind, "test-contract");
                assert_eq!(
                    identity.certificate.as_deref(),
                    Some("SYNTHETIC-CERTIFICATE-EVIDENCE")
                );
                let hash = &identity.certificate_hashes[0];
                assert_eq!(hash.algorithm, CertificateHashAlgorithm::Sha256);
                assert_eq!(
                    (
                        &*hash.issuer_name_hash,
                        &*hash.issuer_key_hash,
                        &*hash.serial_number,
                        &*hash.responder_url
                    ),
                    ("aabb", "ccdd", "1234", "https://ocsp.invalid/test")
                );
            }
            self.entered.notify_one();
            self.release.notified().await;
            self.resolution.clone()
        })
    }
}
fn provider(
    resolution: Result<ChargingIdentityResolution, AuthorizationProviderError>,
    expected_certificate: bool,
) -> Provider {
    Provider {
        resolution,
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        expected_certificate,
    }
}

#[tokio::test]
async fn delayed_resolution_rechecks_expiry_revocation_and_has_a_deadline() {
    let db = Database::new();
    let service = db.recover().await;
    let reference = reference(AUTHORIZE).await;
    allow(
        &service,
        resource(),
        reference.clone(),
        AuthorizationState::Active,
        1,
        Some(timestamp(5)),
    )
    .await;
    let clock = Clock(AtomicU8::new(4));
    let resource = resource();
    let provider = provider(
        Ok(ChargingIdentityResolution::Resolved {
            reference: reference.clone(),
            certificate: None,
        }),
        false,
    );
    let pending = v201::authorize_call(
        AUTHORIZE,
        &resource,
        &service,
        &provider,
        &clock,
        Duration::from_secs(1),
    );
    tokio::pin!(pending);
    tokio::select! { result = &mut pending => panic!("premature result: {result:?}"), () = provider.entered.notified() => {} }
    clock.0.store(5, Ordering::SeqCst);
    provider.release.notify_one();
    assert_eq!(
        pending.await.unwrap()[2]["idTokenInfo"]["status"],
        "Expired"
    );
    let pending = v201::authorize_call(
        AUTHORIZE,
        &resource,
        &service,
        &provider,
        &clock,
        Duration::from_secs(1),
    );
    tokio::pin!(pending);
    tokio::select! { result = &mut pending => panic!("premature result: {result:?}"), () = provider.entered.notified() => {} }
    allow(
        &service,
        resource.clone(),
        reference,
        AuthorizationState::Revoked,
        2,
        None,
    )
    .await;
    provider.release.notify_one();
    assert_eq!(
        pending.await.unwrap()[2]["idTokenInfo"]["status"],
        "Blocked"
    );
    let result = v201::authorize_call(
        AUTHORIZE,
        &resource,
        &service,
        &provider,
        &clock,
        Duration::from_millis(5),
    )
    .await
    .unwrap();
    assert_eq!(result[2], json!({"idTokenInfo":{"status":"Invalid"}}));
}

#[tokio::test]
async fn certificate_evidence_and_provider_denials_remain_independent_of_local_permission() {
    let db = Database::new();
    let service = db.recover().await;
    let reference = reference(AUTHORIZE).await;
    allow(
        &service,
        resource(),
        reference.clone(),
        AuthorizationState::Active,
        1,
        None,
    )
    .await;
    for (certificate, status, expected) in [
        (None, "Invalid", None),
        (
            Some(ChargingCertificateStatus::Revoked),
            "Invalid",
            Some("CertificateRevoked"),
        ),
        (
            Some(ChargingCertificateStatus::Accepted),
            "Accepted",
            Some("Accepted"),
        ),
    ] {
        let provider = provider(
            Ok(ChargingIdentityResolution::Resolved {
                reference: reference.clone(),
                certificate,
            }),
            true,
        );
        provider.release.notify_one();
        let reply = v201::authorize_call(
            CERTIFICATE,
            &resource(),
            &service,
            &provider,
            &Clock(AtomicU8::new(0)),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert_eq!(reply[2]["idTokenInfo"]["status"], status);
        assert_eq!(
            reply[2].get("certificateStatus").and_then(Value::as_str),
            expected
        );
    }
    for (reason, expected) in [
        (ChargingIdentityDenial::Blocked, "Blocked"),
        (
            ChargingIdentityDenial::ConcurrentTransaction,
            "ConcurrentTx",
        ),
        (ChargingIdentityDenial::Expired, "Expired"),
        (ChargingIdentityDenial::Invalid, "Invalid"),
        (ChargingIdentityDenial::NoCredit, "NoCredit"),
        (ChargingIdentityDenial::EvseTypeDenied, "NotAllowedTypeEVSE"),
        (ChargingIdentityDenial::LocationDenied, "NotAtThisLocation"),
        (ChargingIdentityDenial::TimeDenied, "NotAtThisTime"),
        (ChargingIdentityDenial::Unknown, "Unknown"),
    ] {
        let provider = provider(
            Ok(ChargingIdentityResolution::Denied {
                reason,
                certificate: None,
            }),
            false,
        );
        provider.release.notify_one();
        let reply = v201::authorize_call(
            AUTHORIZE,
            &resource(),
            &service,
            &provider,
            &Clock(AtomicU8::new(0)),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert_eq!(reply[2]["idTokenInfo"]["status"], expected);
    }
    let provider = provider(Err(AuthorizationProviderError::Unavailable), false);
    provider.release.notify_one();
    assert_eq!(
        v201::authorize_call(
            AUTHORIZE,
            &resource(),
            &service,
            &provider,
            &Clock(AtomicU8::new(0)),
            Duration::from_secs(1)
        )
        .await
        .unwrap()[2]["idTokenInfo"]["status"],
        "Invalid"
    );
}

#[test]
fn invalid_nested_fields_extensions_and_sensitive_debug_fail_closed() {
    let call = v201::decode_call(CERTIFICATE).unwrap();
    let debug = format!("{call:?}");
    for secret in ["DE-TEST", "SYNTHETIC", "ocsp.invalid", "aabb", "ALTERNATE"] {
        assert!(!debug.contains(secret));
    }
    for (pointer, value) in [
        ("/3/idToken/idToken", json!("")),
        ("/3/idToken/idToken", json!("x".repeat(37))),
        ("/3/idToken/type", json!("RFID")),
        ("/3/certificate", json!("x".repeat(5501))),
        ("/3/iso15118CertificateHashData", json!([])),
        ("/3/idToken/additionalInfo", json!([])),
        (
            "/3/iso15118CertificateHashData/0/issuerNameHash",
            json!("x".repeat(129)),
        ),
        (
            "/3/iso15118CertificateHashData/0/responderURL",
            json!("x".repeat(513)),
        ),
        (
            "/3/idToken/additionalInfo/0/additionalIdToken",
            json!("x".repeat(37)),
        ),
        ("/3/idToken/type", Value::Null),
    ] {
        let mut frame: Value = serde_json::from_slice(CERTIFICATE).unwrap();
        *frame.pointer_mut(pointer).unwrap() = value;
        assert_eq!(
            v201::decode_call(frame.to_string().as_bytes())
                .unwrap_err()
                .kind(),
            DecodeErrorKind::InvalidPayload,
            "{pointer}"
        );
    }
    let mut frame: Value = serde_json::from_slice(AUTHORIZE).unwrap();
    frame[3]["customData"] = json!({"vendorId":"test"});
    assert_eq!(
        v201::decode_call(frame.to_string().as_bytes())
            .unwrap_err()
            .kind(),
        DecodeErrorKind::UnsupportedAction
    );
    frame[3].as_object_mut().unwrap().remove("customData");
    frame[3]["unknown"] = json!(true);
    assert_eq!(
        v201::decode_call(frame.to_string().as_bytes())
            .unwrap_err()
            .kind(),
        DecodeErrorKind::InvalidPayload
    );
}
