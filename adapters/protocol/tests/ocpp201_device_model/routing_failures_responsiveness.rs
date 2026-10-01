use super::helpers::*;
use crate::{endpoint_support::TEST_BOUND, support::*};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::timeout;
use uob_application::*;
use uob_contracts::*;

pub async fn sustained_collection() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (snapshot, _, _, coordinator) = setup_device(&store, running.handle.clone()).await;
    let commands = station_commands(coordinator, &snapshot);
    start_report(
        &mut running,
        &commands,
        report_request(
            &snapshot,
            "responsive-report",
            "GetBaseReport",
            json!({"requestId":70,"reportBase":"FullInventory"}),
            snapshot.station.clone(),
        ),
    )
    .await;
    for sequence in 0..32 {
        notify(
            &mut running,
            notification(
                &format!("responsive-fragment-{sequence}"),
                70,
                sequence,
                true,
                vec![item(json!({"name":"Ctrlr"}), &format!("Probe-{sequence}"))],
            ),
        )
        .await;
        if sequence % 8 == 0 {
            heartbeat(&mut running, &format!("collecting-heartbeat-{sequence}")).await;
            assert_eq!(
                result(&store, "responsive-report")
                    .await
                    .device_model_201
                    .unwrap()
                    .report,
                DeviceReportState201::Pending
            );
        }
    }
    notify(
        &mut running,
        notification(
            "responsive-final",
            70,
            32,
            false,
            vec![item(json!({"name":"Ctrlr"}), "Final")],
        ),
    )
    .await;
    let evidence = terminal(&store, "responsive-report")
        .await
        .device_model_201
        .unwrap();
    assert_eq!(evidence.native_ack, Some(DeviceReportAck201::Accepted));
    let DeviceReportState201::Complete {
        progress,
        fragments,
        items,
    } = evidence.report
    else {
        panic!("responsive complete inventory");
    };
    assert_eq!((progress.fragments, progress.items), (33, 33));
    for (sequence, fragment) in fragments.iter().enumerate() {
        assert_eq!(fragment.sequence, u32::try_from(sequence).unwrap());
        assert_eq!(fragment.more, sequence < 32);
    }
    assert_eq!(items[31].variable.name, "Probe-31");
    assert_eq!(items[32].variable.name, "Final");
    shutdown(running, store).await;
}

fn flood_fragment(sequence: u32) -> Value {
    let items = (0..1536)
        .map(|index| {
            item(
                json!({"name":"Ctrlr"}),
                &format!("Probe-{sequence}-{index}"),
            )
        })
        .collect();
    notification(&format!("flood-{sequence}"), 71, sequence, true, items)
}

pub async fn valid_flood() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (snapshot, _, _, coordinator) = setup_device(&store, running.handle.clone()).await;
    let commands = station_commands(coordinator, &snapshot);
    start_report(
        &mut running,
        &commands,
        report_request(
            &snapshot,
            "flood-report",
            "GetReport",
            json!({"requestId":71}),
            snapshot.station.clone(),
        ),
    )
    .await;
    let first = flood_fragment(0);
    // Stay inside the real endpoint cap; oversized WebSocket frames close the socket.
    assert!(first.to_string().len() < 256 * 1024);
    notify(&mut running, first).await;
    running
        .peer
        .send_text(json!([2, "flood-heartbeat", "Heartbeat", {}]).to_string())
        .await
        .unwrap();

    // Three large valid fragments exceed the item cap but cannot overflow four route slots.
    // Drive the real application handoff concurrently with the remaining flood.
    let producer = async {
        for sequence in 1..3 {
            let frame = flood_fragment(sequence).to_string();
            assert!(frame.len() < 256 * 1024);
            running.peer.send_text(frame).await.unwrap();
        }
    };
    let station_handler = async {
        let incoming = timeout(TEST_BOUND, running.outputs.incoming.receive())
            .await
            .expect("station CALL handoff during flood")
            .expect("application station CALL");
        assert!(matches!(
            incoming.call.observation,
            ChargerObservation::Heartbeat {
                protocol: ProtocolEdition::Ocpp201
            }
        ));
        incoming
            .responder
            .respond(&json!({"currentTime":"2026-09-01T02:00:02Z"}))
            .expect("protected station response admission during flood");
    };
    timeout(TEST_BOUND, async {
        tokio::join!(producer, station_handler)
    })
    .await
    .expect("bounded flood and application response transition");
    let mut expected = std::collections::BTreeSet::from([
        "flood-1".to_owned(),
        "flood-2".to_owned(),
        "flood-heartbeat".to_owned(),
    ]);
    for _ in 0..3 {
        let reply = receive_json(&mut running.peer).await;
        let id = reply[1].as_str().expect("reply identity");
        assert!(expected.remove(id), "duplicate or unrelated flood response");
        let payload = if id == "flood-heartbeat" {
            json!({"currentTime":"2026-09-01T02:00:02Z"})
        } else {
            json!({})
        };
        assert_eq!(reply, json!([3, id, payload]));
    }
    let failed = terminal(&store, "flood-report").await;
    incomplete(&failed, DeviceReportFailure201::ItemLimit, 2, 3072);

    // Continue valid ingress after terminalization; receipts must not reopen the collector.
    for sequence in 3..16 {
        notify(&mut running, flood_fragment(sequence)).await;
        if sequence == 8 {
            heartbeat(&mut running, "continuing-flood-heartbeat").await;
        }
    }
    silence(&mut running).await;
    assert_eq!(result(&store, "flood-report").await, failed);
    heartbeat(&mut running, "post-flood-heartbeat").await;
    shutdown(running, store).await;
}
