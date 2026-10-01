use super::support::Harness;
use crate::support::{accepted, receive_json};
use serde_json::{Value, json};
use uob_contracts::*;

async fn begin(harness: &mut Harness, request: i32) -> String {
    let id = harness.id();
    let payload = json!({"requestId": request, "reportBase": "FullInventory"});
    let submitted = harness.start(&id, "GetBaseReport", payload.clone());
    harness.call(&id, "GetBaseReport", payload).await;
    harness.reply(&id, json!({"status": "Accepted"})).await;
    let acknowledged = submitted.await.unwrap();
    accepted(&acknowledged);
    let evidence = acknowledged.device_model_201.unwrap();
    assert_eq!(evidence.native_ack, Some(DeviceReportAck201::Accepted));
    assert_eq!(evidence.report, DeviceReportState201::Pending);
    id
}

fn item(index: usize) -> Value {
    // Supported-but-unset Actual is schema-valid; no value/characteristics expansion.
    // Distinct short names preserve all 4096 identities below the 1 MiB output cap.
    json!({
        "component": {"name": "A"},
        "variable": {"name": format!("{index:04}")},
        "variableAttribute": [{}],
    })
}

async fn fragment(
    harness: &mut Harness,
    request: i32,
    sequence: u32,
    more: bool,
    items: Vec<Value>,
) {
    let id = harness.id();
    let mut payload = json!({
        "requestId": request,
        "generatedAt": "2026-09-01T02:00:01Z",
        "seqNo": sequence,
        "tbc": more,
    });
    // reportData is optional, but a present empty array is not schema-valid.
    if !items.is_empty() {
        payload["reportData"] = json!(items);
    }
    let frame = json!([2, id, "NotifyReport", payload]).to_string();
    assert!(
        frame.len() <= 256 * 1024,
        "fixture must not exercise the frame cap"
    );
    harness.running.peer.send_text(frame).await.unwrap();
    assert_eq!(
        receive_json(&mut harness.running.peer).await,
        json!([3, id, {}])
    );
    // Give the supervised collector a cooperative turn between acknowledged frames.
    tokio::task::yield_now().await;
}

fn incomplete(
    result: &CommandResult,
    reason: DeviceReportFailure201,
    fragments: u32,
    items: usize,
) {
    accepted(result);
    let evidence = result.device_model_201.as_ref().unwrap();
    assert_eq!(evidence.native_ack, Some(DeviceReportAck201::Accepted));
    let DeviceReportState201::Incomplete {
        reason: actual,
        progress: Some(progress),
    } = &evidence.report
    else {
        panic!("payload-free incomplete report with known accepted counts");
    };
    assert_eq!(*actual, reason);
    assert_eq!(progress.fragments, fragments);
    assert_eq!(progress.items, items);
    assert!(progress.bytes < DEVICE_MODEL_OUTPUT_LIMIT_201);
    let report = serde_json::to_value(&evidence.report).unwrap();
    assert!(
        report.get("items").is_none(),
        "failure must not return partial inventory"
    );
    assert!(
        report.get("fragments").is_none(),
        "failure must not expose partial report payload"
    );
    assert!(evidence.variables.is_empty());
}

#[tokio::test]
async fn report_exact_4096_items_complete_but_4097_fail_across_small_frames_without_partial_inventory()
 {
    let mut harness = Harness::new().await;
    for (request, excess) in [(8100, false), (8101, true)] {
        let id = begin(&mut harness, request).await;
        // Eight 512-item frames isolate the aggregate item boundary from both the
        // native 256 KiB frame cap and the sanitized per-fragment byte cap.
        for sequence in 0..8 {
            let start = usize::try_from(sequence).expect("eight fragment indices fit usize") * 512;
            let items = (start..start + 512).map(item).collect();
            fragment(
                &mut harness,
                request,
                sequence,
                sequence != 7 || excess,
                items,
            )
            .await;
        }
        if excess {
            fragment(&mut harness, request, 8, false, vec![item(4096)]).await;
        }
        let result = harness.terminal(&id).await;
        if excess {
            incomplete(&result, DeviceReportFailure201::ItemLimit, 8, 4096);
        } else {
            accepted(&result);
            let evidence = result.device_model_201.as_ref().unwrap();
            assert_eq!(evidence.native_ack, Some(DeviceReportAck201::Accepted));
            let DeviceReportState201::Complete {
                progress,
                fragments,
                items,
            } = &evidence.report
            else {
                panic!("all 4096 native items must complete");
            };
            assert_eq!(progress.items, 4096);
            assert_eq!(progress.fragments, 8);
            assert!(progress.bytes < DEVICE_MODEL_OUTPUT_LIMIT_201);
            assert_eq!(
                items
                    .iter()
                    .map(|item| item.variable.name.clone())
                    .collect::<Vec<_>>(),
                (0..4096)
                    .map(|index| format!("{index:04}"))
                    .collect::<Vec<_>>()
            );
            for (index, metadata) in fragments.iter().enumerate() {
                assert_eq!(
                    metadata.sequence,
                    u32::try_from(index).expect("eight metadata indices fit u32")
                );
                assert_eq!(metadata.items, 512);
                assert_eq!(metadata.more, index != 7);
            }
            assert!(serde_json::to_vec(&result).unwrap().len() < DEVICE_MODEL_OUTPUT_LIMIT_201);
        }
        harness.no_extra_call().await;
    }
    harness.close().await;
}

#[tokio::test]
async fn report_exact_256_fragments_complete_but_257_fail_without_reopening_on_the_late_final() {
    let mut harness = Harness::new().await;
    let exact_id = begin(&mut harness, 8200).await;
    for sequence in 0..256 {
        fragment(&mut harness, 8200, sequence, sequence != 255, vec![]).await;
    }
    let exact = harness.terminal(&exact_id).await;
    accepted(&exact);
    let evidence = exact.device_model_201.as_ref().unwrap();
    let DeviceReportState201::Complete {
        progress,
        fragments,
        items,
    } = &evidence.report
    else {
        panic!("final fragment at the exact 256 boundary must complete");
    };
    assert_eq!(
        *progress,
        DeviceReportProgress201 {
            fragments: 256,
            items: 0,
            bytes: 0
        }
    );
    assert!(items.is_empty());
    assert_eq!(
        fragments
            .iter()
            .map(|fragment| fragment.sequence)
            .collect::<Vec<_>>(),
        (0..256).collect::<Vec<_>>()
    );
    assert!(fragments[..255].iter().all(|fragment| fragment.more));
    assert!(!fragments[255].more);
    harness.no_extra_call().await;

    let excess_id = begin(&mut harness, 8201).await;
    for sequence in 0..256 {
        fragment(&mut harness, 8201, sequence, true, vec![]).await;
    }
    // A 256th fragment announcing another cannot form a bounded complete report.
    // It is rejected before acceptance; precisely 255 fragments remain accepted.
    let exhausted = harness.terminal(&excess_id).await;
    incomplete(&exhausted, DeviceReportFailure201::FragmentLimit, 255, 0);
    let exhausted_report = exhausted.device_model_201.as_ref().unwrap().report.clone();
    fragment(&mut harness, 8201, 256, false, vec![]).await;
    harness.no_extra_call().await;
    let after_late_final = harness.terminal(&excess_id).await;
    assert_eq!(
        after_late_final.device_model_201.unwrap().report,
        exhausted_report
    );
    harness.close().await;
}
