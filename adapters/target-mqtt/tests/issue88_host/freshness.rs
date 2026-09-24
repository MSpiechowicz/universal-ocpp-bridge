use super::{Host, Pump};
use crate::probe;
use std::{collections::BTreeSet, sync::Arc, time::Duration};
use uob_application::{
    AtomicStoreWrite, DeliveryId, OperationalStore, PageLimit, SnapshotQuery, TargetDelivery,
    TargetDeliveryClass, TargetMessage,
};
use uob_contracts::{Freshness, TargetInstanceId, UtcTimestamp};

/// Commit a new real observation to SQLite after station transactions finish. The same
/// canonical snapshot, not a broker-only forgery, is delivered through the target host.
pub async fn commit_timed_observations(host: &Host, pump: &mut Pump) -> probe::Demo {
    let mut demo: probe::Demo = toml::from_str(include_str!(
        "../../../../tests/ems-mqtt-contract-client/demo.toml"
    ))
    .unwrap();
    let now = time::OffsetDateTime::now_utc();
    let observed_at = UtcTimestamp::new(now);
    let page = host
        .store
        .read_snapshots(SnapshotQuery {
            after: None,
            limit: PageLimit::new(8).unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(
        page.items.len(),
        2,
        "both station states committed before freshness test"
    );
    let mut expected = BTreeSet::new();
    for mut snapshot in page.items {
        let station = snapshot.station.station_id.as_str().to_owned();
        let index = demo
            .scenario
            .iter()
            .position(|case| case.station == station)
            .unwrap();
        snapshot.observed_at = observed_at;
        let end = UtcTimestamp::new(
            now + if index == 0 {
                time::Duration::seconds(3)
            } else {
                time::Duration::minutes(2)
            },
        );
        let mut updated = 0;
        for point in snapshot
            .resources
            .iter_mut()
            .flat_map(|resource| resource.current_values.iter_mut())
            .filter(|point| point.point_id.as_str() == demo.scenario[index].point_id)
        {
            point.observed_at = observed_at;
            point.freshness = Freshness::Fresh {
                valid_until: Some(end),
            };
            updated += 1;
        }
        assert!(updated > 0, "committed measurement missing");
        serde_json::to_value(observed_at)
            .unwrap()
            .as_str()
            .unwrap()
            .clone_into(&mut demo.scenario[index].observed_at);
        demo.scenario[index].freshness = serde_json::to_value(Freshness::Fresh {
            valid_until: Some(end),
        })
        .unwrap();
        demo.scenario[index].expected_current = Some(index == 1);
        let mut write: AtomicStoreWrite<
            serde_json::Value,
            uob_contracts::StationEvent,
            uob_contracts::TransactionSnapshot,
            String,
        > = AtomicStoreWrite::empty();
        write.station_snapshot = Some(snapshot.clone());
        host.store.write_atomic(write).await.unwrap();
        let delivery_id = DeliveryId::new(format!("timed-measurement/{station}")).unwrap();
        expected.insert(delivery_id.as_str().to_owned());
        host.deliveries
            .send(TargetDelivery {
                delivery_id,
                target_instance_id: TargetInstanceId::new("main").unwrap(),
                target_configuration_revision: 1,
                station_ordering_key: snapshot.station.clone(),
                deadline: end,
                class: TargetDeliveryClass::ReplaceableLatestState,
                message: Arc::new(TargetMessage::StationSnapshot(snapshot)),
            })
            .await
            .unwrap();
    }
    // The old retained data must be replaced before starting a delayed subscriber.
    tokio::time::timeout(Duration::from_secs(20), async {
        while !expected.is_empty() {
            let report = pump.expect_reports(1).await.pop().unwrap();
            expected.remove(report.delivery_id.as_str());
        }
    })
    .await
    .expect("both timed measurements broker-acknowledged");
    let expiration = now + time::Duration::seconds(3);
    let remaining = expiration - time::OffsetDateTime::now_utc();
    if remaining.is_positive() {
        tokio::time::sleep(remaining.unsigned_abs() + Duration::from_millis(150)).await;
    }
    demo
}
