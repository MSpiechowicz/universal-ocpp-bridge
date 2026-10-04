use super::{host, support::*};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt};
const PROVISIONED: &str = "PRIVATE-AUTH-111111";
const CASEFOLDED: &str = "private-auth-111111";

#[tokio::test]
async fn genuine_native_authorize_uses_exact_provisioned_bytes_independently_of_list_and_cache() {
    let fixture = fixture();
    let token = fixture.root.join("start-token");
    fs::write(&token, PROVISIONED).unwrap();
    fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
    let path = fixture.root.join("bridge.toml");
    let configuration = fs::read_to_string(&path).unwrap().replace(
        "id='station-a'\n",
        &format!("id='station-a'\nstart_token_file='{}'\n", token.display()),
    );
    fs::write(path, configuration).unwrap();
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = fixture.station("station-a").await;
    boot(&mut socket).await;
    fixture.connected(&client, "station-a").await;
    check_policy(&mut socket, "before").await;
    super::exercise_native_case(
        &client,
        &fixture,
        &mut socket,
        (
            "policy-full",
            "SendLocalList",
            1,
            json!({"status":"Accepted"}),
            true,
        ),
    )
    .await;
    check_policy(&mut socket, "after-full").await;
    authorize(&mut socket, "list-entry-denied", TOKEN, "Invalid").await;
    super::exercise_native_case(
        &client,
        &fixture,
        &mut socket,
        (
            "policy-cache",
            "ClearCache",
            0,
            json!({"status":"Accepted"}),
            true,
        ),
    )
    .await;
    check_policy(&mut socket, "after-cache").await;
    for endpoint in [
        "/api/v1/stations/station-a",
        "/api/v1/commands?station_id=station-a",
    ] {
        let response = client
            .get(fixture.url(endpoint))
            .bearer_auth(READ)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let public = response.json::<Value>().await.unwrap();
        assert_private(&fixture, &public);
        for marker in [PROVISIONED, CASEFOLDED] {
            assert!(!public.to_string().contains(marker));
        }
    }
    for name in ["charging.sqlite3", "charging.sqlite3-wal"] {
        if let Ok(bytes) = fs::read(fixture.root.join("state").join(name)) {
            for marker in [PROVISIONED, CASEFOLDED] {
                assert!(
                    !bytes
                        .windows(marker.len())
                        .any(|window| window == marker.as_bytes())
                );
            }
        }
    }
}
async fn check_policy(socket: &mut host::Socket, phase: &str) {
    authorize(socket, &format!("exact-{phase}"), PROVISIONED, "Accepted").await;
    authorize(socket, &format!("folded-{phase}"), CASEFOLDED, "Invalid").await;
}
async fn authorize(socket: &mut host::Socket, id: &str, token: &str, status: &str) {
    send(socket, json!([2,id,"Authorize",{"idTag":token}])).await;
    assert_eq!(
        receive(socket).await,
        json!([3,id,{"idTagInfo":{"status":status}}])
    );
}
