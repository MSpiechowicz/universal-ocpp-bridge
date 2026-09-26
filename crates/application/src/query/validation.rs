use uob_contracts::{CanonicalResource, ResourceRef};

use super::{TargetQueryAuthorization, TargetQueryPermission};
use crate::{TargetPortError, TargetPortErrorCode, TargetQuery, TargetQueryResult};

pub(super) fn validate_query(
    authorization: &TargetQueryAuthorization,
    query: &TargetQuery,
) -> Result<(), TargetPortError> {
    match query {
        TargetQuery::StationSnapshot(resource) => {
            if resource.resource.is_some() {
                return Err(invalid("query.station_snapshot_requires_station"));
            }
            require_resource(
                authorization,
                TargetQueryPermission::StationSnapshots,
                resource,
            )
        }
        TargetQuery::StationSnapshots(_) => {
            require_permission(authorization, TargetQueryPermission::StationSnapshots)?;
            if authorization.resource_scopes.is_empty() {
                return Err(unauthorized("query.no_resource_scope"));
            }
            Ok(())
        }
        TargetQuery::DataPointDescriptor { resource, .. }
        | TargetQuery::DataPointValue { resource, .. } => {
            require_resource(authorization, TargetQueryPermission::DataPoints, resource)
        }
        TargetQuery::Capabilities(resource) => {
            require_resource(authorization, TargetQueryPermission::Capabilities, resource)
        }
        TargetQuery::CommandResult(_) => {
            require_permission(authorization, TargetQueryPermission::CommandStatus)
        }
        TargetQuery::CommandHistory(query) => {
            require_permission(authorization, TargetQueryPermission::CommandStatus)?;
            if query.station.resource.is_some()
                || authorization
                    .command_history_scope(&query.station)
                    .is_empty()
            {
                return Err(unauthorized("query.command_history_outside_scope"));
            }
            Ok(())
        }
        TargetQuery::RetainedEvents(query) => require_resource(
            authorization,
            TargetQueryPermission::RetainedEvents,
            &query.resource,
        ),
    }
}

pub(super) fn validate_result<E>(
    authorization: &TargetQueryAuthorization,
    query: &TargetQuery,
    result: &TargetQueryResult<E>,
) -> Result<(), TargetPortError> {
    match (query, result) {
        (TargetQuery::StationSnapshot(requested), TargetQueryResult::StationSnapshot(snapshot)) => {
            if let Some(snapshot) = snapshot {
                require_matching_resource(authorization, requested, &snapshot.station)?;
            }
        }
        (TargetQuery::StationSnapshots(query), TargetQueryResult::StationSnapshots(page)) => {
            require_page_bound(query.limit.get(), page.items.len())?;
            for snapshot in &page.items {
                if snapshot.station.resource.is_some()
                    || !authorization.permits_resource(&snapshot.station)
                {
                    return Err(unauthorized("query.snapshot_page_outside_scope"));
                }
            }
        }
        (
            TargetQuery::DataPointDescriptor { resource, point_id },
            TargetQueryResult::DataPointDescriptor(descriptor),
        ) => {
            if let Some(descriptor) = descriptor {
                require_matching_resource(authorization, resource, &descriptor.resource)?;
                if descriptor.point_id != *point_id {
                    return Err(invalid("query.point_descriptor_mismatch"));
                }
            }
        }
        (
            TargetQuery::DataPointValue { point_id, .. },
            TargetQueryResult::DataPointValue(value),
        ) => {
            if value
                .as_ref()
                .is_some_and(|value| value.point_id != *point_id)
            {
                return Err(invalid("query.point_value_mismatch"));
            }
        }
        (TargetQuery::Capabilities(_), TargetQueryResult::Capabilities(_)) => {}
        (TargetQuery::CommandResult(request_id), TargetQueryResult::CommandResult(result)) => {
            if let Some(result) = result {
                if result.return_route.request_id != *request_id {
                    return Err(invalid("query.command_result_mismatch"));
                }
                if !authorization.permits_resource(&result.resource) {
                    return Err(unauthorized("query.command_result_outside_scope"));
                }
            }
        }
        (TargetQuery::CommandHistory(query), TargetQueryResult::CommandHistory(page)) => {
            require_page_bound(query.limit.get(), page.items.len())?;
            let scope = authorization.command_history_scope(&query.station);
            for item in &page.items {
                if !scope.permits(&item.resource, &query.station) {
                    return Err(unauthorized("query.command_history_outside_scope"));
                }
            }
        }
        (TargetQuery::RetainedEvents(query), TargetQueryResult::RetainedEvents(page)) => {
            require_page_bound(query.limit.get(), page.items.len())?;
            for event in &page.items {
                require_matching_resource(authorization, &query.resource, &event.resource)?;
            }
        }
        _ => return Err(invalid("query.response_type_mismatch")),
    }
    Ok(())
}

fn require_permission(
    authorization: &TargetQueryAuthorization,
    permission: TargetQueryPermission,
) -> Result<(), TargetPortError> {
    if authorization.permits(permission) {
        Ok(())
    } else {
        Err(TargetPortError::new(
            TargetPortErrorCode::Unsupported,
            "query.operation_not_granted",
        ))
    }
}

pub(super) fn require_resource(
    authorization: &TargetQueryAuthorization,
    permission: TargetQueryPermission,
    resource: &ResourceRef,
) -> Result<(), TargetPortError> {
    require_permission(authorization, permission)?;
    if authorization.permits_resource(resource) {
        Ok(())
    } else {
        Err(unauthorized("query.resource_outside_scope"))
    }
}

fn require_matching_resource(
    authorization: &TargetQueryAuthorization,
    requested: &ResourceRef,
    returned: &ResourceRef,
) -> Result<(), TargetPortError> {
    if authorization.permits_resource(returned) && same_canonical_resource(requested, returned) {
        Ok(())
    } else {
        Err(unauthorized("query.result_outside_scope"))
    }
}

fn require_page_bound(limit: u16, actual: usize) -> Result<(), TargetPortError> {
    if actual <= usize::from(limit) {
        Ok(())
    } else {
        Err(invalid("query.source_exceeded_page_limit"))
    }
}

pub(super) fn same_canonical_resource(left: &ResourceRef, right: &ResourceRef) -> bool {
    left.bridge_id == right.bridge_id
        && left.station_id == right.station_id
        && same_resource_part(left.resource.as_ref(), right.resource.as_ref())
}

fn same_resource_part(left: Option<&CanonicalResource>, right: Option<&CanonicalResource>) -> bool {
    left == right
}

pub(super) fn unauthorized(context: &'static str) -> TargetPortError {
    TargetPortError::new(TargetPortErrorCode::Unauthorized, context)
}

pub(super) fn invalid(context: &'static str) -> TargetPortError {
    TargetPortError::new(TargetPortErrorCode::InvalidRequest, context)
}
