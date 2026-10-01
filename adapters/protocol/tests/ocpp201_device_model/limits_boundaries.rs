#[path = "limits_boundaries_reports.rs"]
mod reports;
#[path = "limits_boundaries_support.rs"]
mod support;

use self::support::{Harness, payload};
use serde_json::json;
use uob_contracts::*;

#[tokio::test]
async fn advertised_item_limits_admit_exact_batches_and_reject_one_over_without_splitting() {
    let mut harness = Harness::new().await;
    for (index, action) in ["GetVariables", "GetReport"].into_iter().enumerate() {
        let index = i32::try_from(index).expect("two action indices fit native requestId");
        for (name, value) in [("ItemsPerMessage", "2"), ("BytesPerMessage", "65536")] {
            let learned = harness.learn(action, name, value).await;
            assert_eq!(
                learned.device_model_201.unwrap().variables[0].value,
                DeviceValue201 {
                    present: true,
                    redacted: false,
                    empty: false,
                    value: Some(value.to_owned())
                }
            );
        }
        harness
            .query(action, payload(action, 2, 1000 + index))
            .await;
        harness.deny(action, payload(action, 3, 2000 + index)).await;
    }
    harness.close().await;
}

#[tokio::test]
async fn advertised_byte_limits_count_the_complete_escaped_call_at_exact_and_one_over() {
    let mut harness = Harness::new().await;
    // Larger legal names keep subsequent explicit single-entry limit learning below
    // the calibrated GetVariables byte limit. No bootstrap bypass is necessary.
    for action in ["GetVariables", "GetReport"] {
        harness.learn(action, "ItemsPerMessage", "2").await;
        harness.learn(action, "BytesPerMessage", "65536").await;
        let mut calibration = payload(action, 2, 3000);
        let list = if action == "GetVariables" {
            "getVariableData"
        } else {
            "componentVariable"
        };
        for entry in calibration[list].as_array_mut().unwrap() {
            entry["component"]["name"] = json!("C\\\"é-serialization-boundary-component");
        }
        let (_, wire_bytes) = harness.query(action, calibration.clone()).await;
        let encoded =
            serde_json::to_vec(&json!([2, "boundary-000000", action, calibration,])).unwrap();
        assert_eq!(encoded.len(), wire_bytes);
        assert!(wire_bytes > serde_json::to_vec(&calibration).unwrap().len());
        assert!(wire_bytes > encoded.iter().filter(|&&byte| byte & 0xc0 != 0x80).count());
        harness
            .learn(action, "BytesPerMessage", &wire_bytes.to_string())
            .await;

        let mut exact = calibration.clone();
        if action == "GetReport" {
            exact["requestId"] = json!(3001);
        }
        let (_, exact_wire_bytes) = harness.query(action, exact.clone()).await;
        assert_eq!(exact_wire_bytes, wire_bytes);

        let mut excess = exact;
        excess[list][0]["variable"]["name"] = json!("V0000x");
        if action == "GetReport" {
            excess["requestId"] = json!(3002);
        }
        assert_eq!(
            serde_json::to_vec(&json!([2, "boundary-000000", action, excess,]))
                .unwrap()
                .len(),
            wire_bytes + 1
        );
        harness.deny(action, excess).await;
    }
    harness.close().await;
}

#[tokio::test]
async fn invalid_numeric_advertisements_do_not_unlock_unknown_item_or_byte_limits() {
    let mut harness = Harness::new().await;
    for (index, action) in ["GetReport", "GetVariables"].into_iter().enumerate() {
        let index = i32::try_from(index).expect("two action indices fit native requestId");
        harness.learn(action, "BytesPerMessage", "65536").await;
        for invalid in ["0", "-1", "2.5", "2147483648"] {
            let learned = harness.learn(action, "ItemsPerMessage", invalid).await;
            let item = &learned.device_model_201.as_ref().unwrap().variables[0];
            assert_eq!(
                item.value,
                DeviceValue201 {
                    present: true,
                    redacted: true,
                    empty: false,
                    value: None,
                }
            );
            harness.deny(action, payload(action, 2, 4000 + index)).await;
        }
        harness.learn(action, "ItemsPerMessage", "2").await;
        harness
            .query(action, payload(action, 2, 5000 + index))
            .await;
    }
    harness.close().await;

    // Separate actual connections leave BytesPerMessage unknown while ItemsPerMessage
    // is explicitly known, so rejecting malformed byte advertisements is independent.
    for action in ["GetReport", "GetVariables"] {
        let mut harness = Harness::new().await;
        harness.learn(action, "ItemsPerMessage", "2").await;
        for invalid in ["0", "1e6", " 65536", "2147483648"] {
            let learned = harness.learn(action, "BytesPerMessage", invalid).await;
            assert!(
                learned.device_model_201.unwrap().variables[0]
                    .value
                    .redacted
            );
            harness.deny(action, payload(action, 2, 6000)).await;
        }
        harness.learn(action, "BytesPerMessage", "65536").await;
        harness.query(action, payload(action, 2, 6001)).await;
        harness.close().await;
    }
}

#[tokio::test]
async fn unknown_limits_allow_single_bootstrap_but_learned_limits_reset_on_real_reconnect() {
    let mut harness = Harness::new().await;
    for action in ["GetVariables", "GetReport"] {
        harness.deny(action, payload(action, 2, 7000)).await;
    }
    // The first explicit limit request is one entry on a genuinely unknown connection.
    let first = harness.learn("GetVariables", "ItemsPerMessage", "2").await;
    let first_context = first.device_model_201.unwrap();
    assert_eq!(first_context.generation, 1);
    harness
        .deny("GetVariables", payload("GetVariables", 2, 7001))
        .await;
    harness
        .learn("GetVariables", "BytesPerMessage", "65536")
        .await;
    harness.learn("GetReport", "ItemsPerMessage", "2").await;
    harness.learn("GetReport", "BytesPerMessage", "65536").await;
    for action in ["GetVariables", "GetReport"] {
        harness.query(action, payload(action, 2, 7002)).await;
    }

    let mut harness = harness.reconnect().await;
    // Keep the same SQLite store and station; only the socket and generation change.
    // No command or implicit discovery may appear spontaneously on the fresh socket.
    harness.no_extra_call().await;
    for action in ["GetVariables", "GetReport"] {
        harness.deny(action, payload(action, 2, 7003)).await;
    }
    let bootstrap = harness.learn("GetVariables", "ItemsPerMessage", "2").await;
    let context = bootstrap.device_model_201.unwrap();
    assert_ne!(context.connection, first_context.connection);
    assert_eq!(context.generation, 2);
    harness
        .deny("GetVariables", payload("GetVariables", 2, 7004))
        .await;
    harness
        .learn("GetVariables", "BytesPerMessage", "65536")
        .await;
    harness
        .query("GetVariables", payload("GetVariables", 2, 7005))
        .await;
    // Relearning GetVariables must not restore the old GetReport limit cache.
    harness
        .deny("GetReport", payload("GetReport", 2, 7006))
        .await;
    harness.close().await;
}
