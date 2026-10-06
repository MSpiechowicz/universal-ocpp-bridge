pub use crate::host::{
    CONTROL, Fixture, PRIVILEGED, Socket, begin, boot, client, completed, no_call, receive, result,
    send, station_call,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use rustls_pki_types::{CertificateDer, UnixTime, pem::PemObject};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, os::unix::fs::PermissionsExt, path::Path};
use webpki::{EndEntityCert, KeyUsage};

/// DER content of the id-kp-codeSigning OID 1.3.6.1.5.5.7.3.3.
const CODE_SIGNING: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x03];
pub const LEGACY_IMAGE: &str = "station-fw-1.1.bin";
pub const SIGNED_IMAGE: &str = "station-fw-2.0-signed.bin";

pub fn image(seed: u8) -> Vec<u8> {
    (0..70_001_u32)
        .map(|index| u8::try_from(index % 251).unwrap() ^ seed)
        .collect()
}

fn private(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn vacant_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Station A gets exactly one firmware family; station B stays without firmware.
pub fn fixture(signed: bool, trigger: bool) -> (Fixture, u16) {
    let fixture = Fixture::new();
    let port = vacant_port();
    let spool = fixture.root.join("artifact-spool");
    fs::create_dir(&spool).unwrap();
    fs::set_permissions(&spool, fs::Permissions::from_mode(0o700)).unwrap();
    private(&fixture.root.join("legacy.bin"), &image(1));
    private(&fixture.root.join("signed.bin"), &image(2));
    let catalog = json!({"artifacts":[
        {"reference":LEGACY_IMAGE,"file":fixture.root.join("legacy.bin"),"signed":false},
        {"reference":SIGNED_IMAGE,"file":fixture.root.join("signed.bin"),"signed":true}
    ]});
    private(
        &fixture.root.join("catalog.json"),
        catalog.to_string().as_bytes(),
    );
    let path = fixture.root.join("bridge.toml");
    let family = if signed {
        "signed_update_firmware=true"
    } else {
        "update_firmware=true"
    };
    let options = format!(
        "{family}\nfirmware_job_timeout_seconds=600{}",
        if trigger {
            "\ntrigger_message=true"
        } else {
            ""
        }
    );
    let mut config = fs::read_to_string(&path)
        .unwrap()
        .replace("get_composite_schedule=true", &options);
    config.push_str(&format!(
        "[charging.firmware]\nlisten_addr='127.0.0.1:{port}'\nspool_directory='{}'\n\
         catalog_file='{}'\nmanufacturer_root_file='{}'\n",
        spool.display(),
        fixture.root.join("catalog.json").display(),
        fixture.root.join("manufacturer-root.pem").display(),
    ));
    fs::write(path, config).unwrap();
    (fixture, port)
}

pub fn station() -> Value {
    json!({"bridge_id":"bridge-1","station_id":"station-a"})
}

pub fn legacy_command(id: &str, reference: &str) -> Value {
    json!({"request_id":id,"resource":station(),"operation":{"kind":"ocpp","parameters":{
        "protocol":"ocpp16j","action":"UpdateFirmware",
        "payload_schema":"urn:uob:ocpp16:UpdateFirmwareReference:1",
        "payload":{"artifactReference":reference,"retrieveDate":"2026-01-01T00:00:00Z","retries":2,"retryInterval":30}
    }},"expires_at":"2099-01-01T00:00:00Z"})
}

pub fn signed_command(id: &str, request_id: i32, reference: &str) -> Value {
    json!({"request_id":id,"resource":station(),"operation":{"kind":"ocpp","parameters":{
        "protocol":"ocpp16j","action":"SignedUpdateFirmware",
        "payload_schema":"urn:uob:ocpp16:SignedUpdateFirmwareReference:1",
        "payload":{"requestId":request_id,"artifactReference":reference,
            "retrieveDateTime":"2026-01-01T00:00:00Z","installDateTime":"2026-01-01T00:05:00Z"}
    }},"expires_at":"2099-01-01T00:00:00Z"})
}

/// Downloads exactly what the station was told to fetch and checks the public evidence.
pub async fn download(location: &str, artifact: &Value, expected: &[u8]) {
    let bytes = client()
        .get(location)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(bytes.as_ref(), expected);
    let digest = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(artifact["sha256"], digest);
    assert_eq!(artifact["size_bytes"], expected.len());
    assert_eq!(artifact["test_only"], true);
}

/// An independent station-side check: code-signing leaf under the published manufacturer root
/// and RSA-PSS SHA-256 over the complete image (L01.FR.04, L01.FR.12).
pub fn station_accepts(fixture: &Fixture, firmware: &Value, image: &[u8]) -> bool {
    let root_pem = fs::read(fixture.root.join("manufacturer-root.pem")).unwrap();
    let root = CertificateDer::from_pem_slice(&root_pem).unwrap();
    let anchors = [webpki::anchor_from_trusted_cert(&root).unwrap()];
    let leaf =
        CertificateDer::from_pem_slice(firmware["signingCertificate"].as_str().unwrap().as_bytes())
            .unwrap();
    let leaf = EndEntityCert::try_from(&leaf).unwrap();
    let signature = STANDARD
        .decode(firmware["signature"].as_str().unwrap())
        .unwrap();
    leaf.verify_for_usage(
        webpki::ALL_VERIFICATION_ALGS,
        &anchors,
        &[],
        UnixTime::now(),
        KeyUsage::required(CODE_SIGNING),
        None,
        None,
    )
    .is_ok()
        && leaf
            .verify_signature(
                webpki::aws_lc_rs::RSA_PSS_2048_8192_SHA256_LEGACY_KEY,
                image,
                &signature,
            )
            .is_ok()
}

/// A refused command returns its durable or synthetic result with a client error status.
pub async fn submit_refused(
    client: &reqwest::Client,
    fixture: &Fixture,
    body: Value,
) -> (u16, Value) {
    let response = client
        .post(fixture.url("/api/v1/commands"))
        .bearer_auth(PRIVILEGED)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

pub async fn notify(socket: &mut Socket, id: &str, action: &str, payload: Value) {
    assert_eq!(station_call(socket, id, action, payload).await, json!({}));
}

pub async fn job_state(client: &reqwest::Client, fixture: &Fixture, id: &str) -> Value {
    result(client, fixture, id).await["firmware_16"]["job"]["state"].clone()
}
