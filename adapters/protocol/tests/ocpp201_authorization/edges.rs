use super::*;

#[tokio::test]
async fn valid_certificate_cannot_override_persisted_revocation() {
    let db = Database::new();
    let service = db.recover().await;
    let reference = reference(AUTHORIZE).await;
    allow(
        &service,
        resource(),
        reference.clone(),
        AuthorizationState::Revoked,
        1,
        None,
    )
    .await;
    let provider = provider(
        Ok(ChargingIdentityResolution::Resolved {
            reference,
            certificate: Some(ChargingCertificateStatus::Accepted),
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
    assert_eq!(
        reply[2],
        json!({"certificateStatus":"Accepted", "idTokenInfo":{"status":"Blocked"}})
    );
}

#[test]
fn array_bounds_and_required_fields_are_enforced() {
    for (pointer, limit) in [
        ("/3/idToken/additionalInfo", 16),
        ("/3/iso15118CertificateHashData", 4),
    ] {
        let mut frame: Value = serde_json::from_slice(CERTIFICATE).unwrap();
        let item = frame.pointer(pointer).unwrap()[0].clone();
        *frame.pointer_mut(pointer).unwrap() = json!(vec![item.clone(); limit]);
        assert!(v201::decode_call(frame.to_string().as_bytes()).is_ok());
        *frame.pointer_mut(pointer).unwrap() = json!(vec![item; limit + 1]);
        assert_eq!(
            v201::decode_call(frame.to_string().as_bytes())
                .unwrap_err()
                .kind(),
            DecodeErrorKind::InvalidPayload
        );
    }
    for frame in [
        r#"[2,"missing","Authorize",{}]"#,
        r#"[2,"wrong-version","Authorize",{"idTag":"LOCAL-USER-1"}]"#,
        r#"[2,"missing-type","Authorize",{"idToken":{"idToken":"LOCAL-USER-1"}}]"#,
    ] {
        assert_eq!(
            v201::decode_call(frame.as_bytes()).unwrap_err().kind(),
            DecodeErrorKind::InvalidPayload
        );
    }
}
