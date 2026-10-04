use super::socket::{accept, boot, closed, receive, send};
use super::{BOUND, PrivateDirectory, Process, TRANSACTION_ID, events, step};
use serde_json::json;
use tokio::net::TcpListener;
use tokio::time::timeout;

pub(super) async fn enabled_list_after_process_restart(directory: &PrivateDirectory) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let config = directory.write("enabled-configuration.toml", &format!(
        "schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://{address}'\nocpp_version='1.6'\nrequest_timeout_ms=2000\n[stations.local_authorization]\nprivate_state_file='{}'\nlist_supported=true\ncache_supported=true\n", directory.settings().private_state_file));
    let mut scenario = "schema_version=1\nseed=111\n".to_owned();
    scenario += &step("connect", "connect", "");
    scenario += &step(
        "boot",
        "boot",
        "fixture_id='sim.ocpp16.boot.accepted'\npayload={chargePointVendor='UOB',chargePointModel='Simulator'}",
    );
    scenario += &step(
        "assert-recovered",
        "assert_local_authorization",
        "expect_response={listVersion=7,listEntries=1,cacheEntries=1,offlineRecords=0}",
    );
    scenario += &step("offline", "csms_offline", "");
    scenario += &step(
        "blocked-list-priority",
        "offline_start",
        "payload={connectorId=1,idTag='cache-private',meterStart=800,timestamp='2026-10-04T00:00:03Z'}\nexpect_response={accepted=false}",
    );
    scenario += &step(
        "assert-no-new-record",
        "assert_local_authorization",
        "expect_response={listEntries=1,cacheEntries=1,offlineRecords=0}",
    );
    let scenario = directory.write("enabled-scenario.toml", &scenario);
    let peer = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        let start = receive(&mut socket).await;
        assert_eq!(start[2], "StartTransaction");
        assert_eq!(start[3]["idTag"], "CACHE-PRIVATE");
        assert_eq!(start[3]["meterStart"], 600);
        assert_eq!(start[3]["timestamp"], "2026-10-04T00:00:01Z");
        // The original completed offline transaction is reported even if the
        // current central decision denies it; its original stop is not replaced.
        send(
            &mut socket,
            json!([3,start[1],{"transactionId":TRANSACTION_ID,
            "idTagInfo":{"status":"Blocked"}}]),
        )
        .await;
        let stop = receive(&mut socket).await;
        assert_eq!(stop[2], "StopTransaction");
        assert_eq!(stop[3]["transactionId"], TRANSACTION_ID);
        assert_eq!(stop[3]["meterStop"], 700);
        assert_eq!(stop[3]["timestamp"], "2026-10-04T00:00:02Z");
        assert_eq!(stop[3]["reason"], "Local");
        send(&mut socket, json!([3, stop[1], {}])).await;
        closed(&mut socket).await;
    });
    let output = Process::start(&config, &scenario).finish().await;
    let observed = events(&output);
    timeout(BOUND, peer).await.unwrap().unwrap();
    assert!(
        output.status.success(),
        "retained list/cache overlap must recover under enabled-list priority"
    );
    assert_eq!(observed.last().unwrap()["event"], "run_passed");
    let state = directory.state();
    assert_eq!(
        state["cache"]["cache-private"]["idTagInfo"]["status"],
        "Accepted"
    );
    assert_eq!(
        state["list"]["cache-private"]["idTagInfo"]["status"],
        "Blocked"
    );
    assert_eq!(state["offline"].as_array().unwrap().len(), 0);
}
