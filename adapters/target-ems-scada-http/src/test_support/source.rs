//! A canonical source that answers exactly the queries the host is expected to serve.

use uob_application::{
    CanonicalQuerySource, Page, RetainedEventQuery, SnapshotCursor, TargetPortError,
    TargetPortErrorCode, TargetPortFuture, TargetQuery, TargetQueryAuthorization,
    TargetQueryResult, TargetRetainedEventStream,
};
use uob_contracts::{DataPointDescriptor, DataPointValue, ResourceRef, StationSnapshot};

use super::fixtures;

/// Canonical records a fixture host holds, paged the way authoritative storage pages them.
#[derive(Default)]
pub(crate) struct CanonicalFixtures {
    stations: Vec<StationSnapshot>,
}

impl CanonicalFixtures {
    /// Holds both OCPP resource models plus a station outside every credential's scope.
    pub(crate) fn both_resource_models() -> Self {
        Self {
            stations: vec![
                fixtures::ocpp16_station(),
                fixtures::ocpp201_station(),
                fixtures::unscoped_station(),
            ],
        }
    }

    /// Holds one station so a bounded point page must resume inside it.
    pub(crate) fn single_station() -> Self {
        Self {
            stations: vec![fixtures::ocpp16_station()],
        }
    }

    /// Returns the snapshots after an opaque cursor, honouring the requested page bound.
    fn page(
        &self,
        after: Option<&SnapshotCursor>,
        limit: usize,
    ) -> Page<StationSnapshot, SnapshotCursor> {
        let start = match after {
            None => 0,
            Some(cursor) => cursor
                .as_str()
                .strip_prefix("snapshot:")
                .and_then(|index| index.parse::<usize>().ok())
                .unwrap_or(self.stations.len()),
        };
        let end = start.saturating_add(limit).min(self.stations.len());
        Page {
            items: self.stations[start.min(self.stations.len())..end].to_vec(),
            next_cursor: (end < self.stations.len())
                .then(|| SnapshotCursor::new(format!("snapshot:{end}")).expect("cursor")),
        }
    }

    fn descriptor(&self, query: &TargetQuery) -> Option<DataPointDescriptor> {
        let TargetQuery::DataPointDescriptor { resource, point_id } = query else {
            return None;
        };
        self.stations
            .iter()
            .flat_map(|station| station.resources.iter())
            .flat_map(|charging| charging.data_points.iter())
            .find(|descriptor| {
                same_resource(&descriptor.resource, resource) && descriptor.point_id == *point_id
            })
            .cloned()
    }

    fn value(&self, query: &TargetQuery) -> Option<DataPointValue> {
        let TargetQuery::DataPointValue { resource, point_id } = query else {
            return None;
        };
        self.stations
            .iter()
            .flat_map(|station| {
                station
                    .resources
                    .iter()
                    .map(|charging| (&charging.resource, &charging.current_values))
                    .chain(std::iter::once((&station.station, &station.current_values)))
            })
            .filter(|(owner, _)| same_resource(owner, resource))
            .flat_map(|(_, values)| values.iter())
            .find(|value| value.point_id == *point_id)
            .cloned()
    }
}

/// Compares canonical identity the way the application does, ignoring the native address that a
/// canonical record retains only as protocol evidence.
fn same_resource(left: &ResourceRef, right: &ResourceRef) -> bool {
    left.bridge_id == right.bridge_id
        && left.station_id == right.station_id
        && left.resource == right.resource
}

impl CanonicalQuerySource<()> for CanonicalFixtures {
    fn query<'a>(
        &'a self,
        _authorization: &'a TargetQueryAuthorization,
        query: TargetQuery,
    ) -> TargetPortFuture<'a, TargetQueryResult<()>> {
        Box::pin(async move {
            match &query {
                TargetQuery::StationSnapshots(request) => Ok(TargetQueryResult::StationSnapshots(
                    self.page(request.after.as_ref(), usize::from(request.limit.get())),
                )),
                TargetQuery::StationSnapshot(resource) => Ok(TargetQueryResult::StationSnapshot(
                    self.stations
                        .iter()
                        .find(|station| same_resource(&station.station, resource))
                        .cloned(),
                )),
                TargetQuery::DataPointDescriptor { .. } => Ok(
                    TargetQueryResult::DataPointDescriptor(self.descriptor(&query)),
                ),
                TargetQuery::DataPointValue { .. } => {
                    Ok(TargetQueryResult::DataPointValue(self.value(&query)))
                }
                _ => Err(TargetPortError::new(
                    TargetPortErrorCode::Unsupported,
                    "query.unsupported",
                )),
            }
        })
    }

    fn subscribe_retained_events<'a>(
        &'a self,
        _authorization: &'a TargetQueryAuthorization,
        _query: RetainedEventQuery,
    ) -> TargetPortFuture<'a, TargetRetainedEventStream<()>> {
        Box::pin(async {
            Err(TargetPortError::new(
                TargetPortErrorCode::Unsupported,
                "query.unsupported",
            ))
        })
    }
}
