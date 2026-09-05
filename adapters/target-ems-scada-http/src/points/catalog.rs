use std::collections::BTreeMap;

use serde::Serialize;
use uob_contracts::{
    CanonicalResource, DataPointDescriptor, DataPointValue, PointId, ResourceRef, StationSnapshot,
};

/// One canonical point as the integration API renders it.
///
/// The descriptor and the value are the canonical contract objects themselves, so units, access
/// mode, exact decimals, both timestamps, quality, and freshness reach an EMS client exactly as
/// the bridge recorded them. This adapter re-derives none of them.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct PointView {
    /// Stable point identity within its owning resource.
    pub(crate) point_id: PointId,
    /// Canonical resource owning the point, preserving EVSE or connector identity.
    pub(crate) resource: ResourceRef,
    /// Canonical description, when the snapshot or the source carries one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) descriptor: Option<DataPointDescriptor>,
    /// Latest observed value, when one has been observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) value: Option<DataPointValue>,
}

/// Flattens one canonical station snapshot into its stable point list.
///
/// Station-level meters (OCPP 1.6 connector zero and OCPP 2.0.1 EVSE zero) are listed against the
/// station itself; every other point keeps the EVSE or connector that owns it. Points are ordered
/// by owning resource and then by identity so a bounded page can be resumed deterministically.
pub(crate) fn points(
    snapshot: &StationSnapshot,
    filter: Option<&CanonicalResource>,
) -> Vec<PointView> {
    let mut points = Vec::new();
    if filter.is_none() {
        collect(
            &mut points,
            &snapshot.station,
            &[],
            &snapshot.current_values,
        );
    }
    for resource in &snapshot.resources {
        if !filter.is_none_or(|filter| contains(filter, resource.resource.resource.as_ref())) {
            continue;
        }
        collect(
            &mut points,
            &resource.resource,
            &resource.data_points,
            &resource.current_values,
        );
    }
    points
}

/// Returns whether a requested resource filter covers one canonical resource.
///
/// An EVSE named without a connector covers the EVSE and every connector below it, so the two
/// OCPP resource models stay addressable with the identifiers each one actually uses.
fn contains(filter: &CanonicalResource, candidate: Option<&CanonicalResource>) -> bool {
    let Some(candidate) = candidate else {
        return false;
    };
    match (filter, candidate) {
        (
            CanonicalResource::Evse {
                evse_id,
                connector_id: None,
            },
            CanonicalResource::Evse {
                evse_id: candidate_evse,
                ..
            },
        ) => evse_id == candidate_evse,
        (filter, candidate) => filter == candidate,
    }
}

/// Joins the descriptors and values one canonical resource carries.
///
/// A resource may describe a point it has not observed yet, and may observe a station-level point
/// the snapshot does not describe, so both directions produce an entry. The first record wins for
/// a repeated identity, matching how the broker-based target builds the same catalog.
fn collect(
    points: &mut Vec<PointView>,
    resource: &ResourceRef,
    descriptors: &[DataPointDescriptor],
    values: &[DataPointValue],
) {
    let mut joined: BTreeMap<&PointId, (Option<&DataPointDescriptor>, Option<&DataPointValue>)> =
        BTreeMap::new();
    for descriptor in descriptors {
        joined
            .entry(&descriptor.point_id)
            .or_default()
            .0
            .get_or_insert(descriptor);
    }
    for value in values {
        joined
            .entry(&value.point_id)
            .or_default()
            .1
            .get_or_insert(value);
    }
    points.extend(
        joined
            .into_iter()
            .map(|(point_id, (descriptor, value))| PointView {
                point_id: point_id.clone(),
                resource: resource.clone(),
                descriptor: descriptor.cloned(),
                value: value.cloned(),
            }),
    );
}
