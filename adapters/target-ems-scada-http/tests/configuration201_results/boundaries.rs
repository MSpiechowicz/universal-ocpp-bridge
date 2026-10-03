use super::*;
use host::STATION_OPERATOR;

#[tokio::test]
async fn configuration_result_reads_preserve_principal_origin_and_exact_resource_boundaries() {
    let cases = [
        (
            "own",
            fixtures::target_origin("operator", "main"),
            "station-a",
        ),
        (
            "management",
            json!({"kind":"management","principal_id":"operator"}),
            "station-a",
        ),
        (
            "foreign-target",
            fixtures::target_origin("operator", "other"),
            "station-a",
        ),
        (
            "foreign-principal",
            fixtures::target_origin("other", "main"),
            "station-a",
        ),
        (
            "station-own",
            fixtures::target_origin("station-operator", "main"),
            "station-a",
        ),
        (
            "station-outside",
            fixtures::target_origin("station-operator", "main"),
            "station-b",
        ),
        (
            "host-outside",
            fixtures::target_origin("operator", "main"),
            "outside",
        ),
    ];
    let mut host = Host::start(
        cases
            .into_iter()
            .map(|(id, origin, station)| result(id, origin, station, network("Accepted")))
            .collect(),
        true,
    )
    .await;
    assert_eq!(host.get("/bridge/v1/commands/own", OPERATOR).await.0, 200);
    assert_eq!(host.get("/bridge/v1/commands/own", READER).await.0, 403);
    assert_eq!(
        host.get("/bridge/v1/commands/station-own", STATION_OPERATOR)
            .await
            .0,
        200
    );
    for (id, credential) in [
        ("management", OPERATOR),
        ("foreign-target", OPERATOR),
        ("foreign-principal", OPERATOR),
        ("station-outside", STATION_OPERATOR),
        ("host-outside", OPERATOR),
    ] {
        let (status, body) = host
            .get(&format!("/bridge/v1/commands/{id}"), credential)
            .await;
        assert_eq!(status, 404);
        assert_eq!(body, json!({"error":"ems_scada_http.resource_not_found"}));
    }
    host.stop().await;
    let mut ungranted = Host::start(vec![own("own", network("Accepted"))], false).await;
    let (status, body) = ungranted.get("/bridge/v1/commands/own", OPERATOR).await;
    assert_eq!(status, 501);
    assert_eq!(body["error"], "ems_scada_http.operation_not_supported");
    assert!(body.get("configuration_201").is_none());
    ungranted.stop().await;
}

#[tokio::test]
async fn protected_native_writes_remain_denied_before_ems_host_admission() {
    let mut host = Host::start(vec![], true).await;
    for (action, schema, payload) in [
        (
            "SetVariables",
            uob_contracts::SET_VARIABLES_REFERENCE_SCHEMA_201,
            json!({"setVariableData":[{"component":{"name":"VendorCtrlr"},
                "variable":{"name":"PrivateSetting"},"valueReference":format!("cfg201:{}", "a".repeat(64))}]}),
        ),
        (
            "SetNetworkProfile",
            uob_contracts::SET_NETWORK_PROFILE_REFERENCE_SCHEMA_201,
            json!({"configurationSlot":0,"profileReference":format!("cfg201:{}", "b".repeat(64))}),
        ),
    ] {
        let response = host.client.post(format!("{}/bridge/v1/commands", host.base)).bearer_auth(OPERATOR)
            .json(&json!({"request_id":format!("denied-{action}"),
                "resource":{"bridge_id":"site-01","station_id":"station-a"},
                "operation":{"kind":"ocpp","parameters":{"protocol":"ocpp201","action":action,
                    "payload_schema":schema,"payload":payload}},"expires_at":"2099-01-01T00:00:00Z"}))
            .send().await.unwrap();
        assert_eq!(response.status(), 403);
        assert_eq!(
            response.json::<Value>().await.unwrap(),
            json!({"error":"ems_scada_http.permission_denied"})
        );
    }
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(50),
            host.driver.next_command()
        )
        .await
        .is_err()
    );
    host.stop().await;
}

#[tokio::test]
async fn oversized_configuration_result_fails_without_truncation_and_small_result_remains_readable()
{
    let large = own("large", variables(&vec!["Accepted"; 4096]));
    assert!(serde_json::to_vec(&large).unwrap().len() > 256 * 1024);
    let mut host = Host::start(
        vec![large, own("small", variables(&["RebootRequired"]))],
        true,
    )
    .await;
    let (_, capabilities) = host.get("/bridge/v1/capabilities", OPERATOR).await;
    assert_eq!(capabilities["limits"]["maximum_message_bytes"], 256 * 1024);
    let (status, body) = host.get("/bridge/v1/commands/large", OPERATOR).await;
    assert_eq!(status, 413);
    assert_eq!(body, json!({"error":"ems_scada_http.payload_too_large"}));
    let (status, body) = host.get("/bridge/v1/commands/small", OPERATOR).await;
    assert_eq!(status, 200);
    assert_eq!(body["configuration_201"], variables(&["RebootRequired"]));
    host.stop().await;
}
