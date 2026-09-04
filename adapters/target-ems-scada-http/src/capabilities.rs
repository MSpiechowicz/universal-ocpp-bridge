use serde::Serialize;
use uob_application::{
    AccessPermission, AccessResourceScope, DeliverySemantic, PageLimit, TargetDescriptor,
    TargetMessageClass,
};

use crate::configuration::IntegrationPrincipal;
use uob_contracts::{ContractVersion, Operation};

/// One integration resource this build actually serves.
pub(crate) struct IntegrationResource {
    pub(crate) name: &'static str,
    pub(crate) path: &'static str,
    pub(crate) operations: &'static [&'static str],
}

/// The exact resource table the router mounts.
///
/// The capability response is generated from this table, so an advertised resource cannot drift
/// away from a route that exists, and a route cannot appear without being advertised.
pub(crate) const IMPLEMENTED_RESOURCES: &[IntegrationResource] = &[
    IntegrationResource {
        name: "capabilities",
        path: "/bridge/v1/capabilities",
        operations: &["read"],
    },
    IntegrationResource {
        name: "stations",
        path: "/bridge/v1/stations",
        operations: &["read"],
    },
    IntegrationResource {
        name: "station",
        path: "/bridge/v1/stations/{station_id}",
        operations: &["read"],
    },
    IntegrationResource {
        name: "points",
        path: "/bridge/v1/points",
        operations: &["read"],
    },
    IntegrationResource {
        name: "point",
        path: "/bridge/v1/points/{point_id}",
        operations: &["read"],
    },
];

/// Versioned description of the integration surface, generated from the target descriptor.
#[derive(Serialize)]
pub(crate) struct CapabilityDocument {
    contract_version: ContractVersion,
    target: TargetView,
    resources: Vec<ResourceView>,
    outbound_message_classes: Vec<&'static str>,
    inbound_operations: Vec<Operation>,
    delivery_semantics: Vec<&'static str>,
    optional_capabilities: Vec<String>,
    limits: LimitsView,
    caller: CallerView,
}

#[derive(Serialize)]
struct TargetView {
    kind: String,
    instance_id: String,
}

#[derive(Serialize)]
struct ResourceView {
    name: &'static str,
    path: &'static str,
    operations: &'static [&'static str],
}

// The wire names deliberately mirror the descriptor's own limit names so an integration client
// can match the capability response against the published contract field for field.
#[allow(clippy::struct_field_names)]
#[derive(Serialize)]
struct LimitsView {
    maximum_message_bytes: usize,
    maximum_in_flight_deliveries: usize,
    maximum_in_flight_commands: usize,
    maximum_request_bytes: usize,
    maximum_concurrent_requests: usize,
    maximum_page_size: u16,
    default_page_size: u16,
    maximum_station_scan: u16,
}

/// Permissions and canonical resources the authenticated caller actually holds.
#[derive(Serialize)]
struct CallerView {
    permissions: Vec<&'static str>,
    resource_scopes: Vec<ScopeView>,
}

#[derive(Serialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
enum ScopeView {
    Bridge {
        bridge_id: String,
    },
    Station {
        bridge_id: String,
        station_id: String,
    },
    Resource {
        bridge_id: String,
        station_id: String,
    },
}

/// Bounds advertised beside the descriptor limits but owned by the listener.
#[derive(Clone, Copy)]
pub(crate) struct ListenerLimits {
    pub(crate) maximum_request_bytes: usize,
    pub(crate) maximum_concurrent_requests: usize,
    /// Station snapshots one bounded point page may inspect.
    pub(crate) station_scan_limit: PageLimit,
}

impl CapabilityDocument {
    /// Builds the response from the same descriptor supervision reads and the caller's grant.
    pub(crate) fn new(
        descriptor: &TargetDescriptor,
        listener: ListenerLimits,
        principal: Option<&IntegrationPrincipal>,
    ) -> Self {
        Self {
            contract_version: descriptor.contract_version,
            target: TargetView {
                kind: descriptor.kind.as_str().to_owned(),
                instance_id: descriptor.instance_id.as_str().to_owned(),
            },
            resources: IMPLEMENTED_RESOURCES
                .iter()
                .map(|resource| ResourceView {
                    name: resource.name,
                    path: resource.path,
                    operations: resource.operations,
                })
                .collect(),
            outbound_message_classes: descriptor
                .outbound_message_classes
                .iter()
                .map(|class| message_class(*class))
                .collect(),
            inbound_operations: descriptor.inbound_operations.clone(),
            delivery_semantics: descriptor
                .delivery_semantics
                .iter()
                .map(|semantic| delivery_semantic(*semantic))
                .collect(),
            optional_capabilities: descriptor
                .optional_capabilities
                .iter()
                .map(|capability| capability.0.clone())
                .collect(),
            limits: LimitsView {
                maximum_message_bytes: descriptor.limits.maximum_message_bytes,
                maximum_in_flight_deliveries: descriptor.limits.maximum_in_flight_deliveries,
                maximum_in_flight_commands: descriptor.limits.maximum_in_flight_commands,
                maximum_request_bytes: listener.maximum_request_bytes,
                maximum_concurrent_requests: listener.maximum_concurrent_requests,
                maximum_page_size: crate::request::maximum_page_size(),
                default_page_size: crate::request::DEFAULT_PAGE_SIZE,
                maximum_station_scan: listener.station_scan_limit.get(),
            },
            caller: CallerView {
                permissions: principal
                    .map(|principal| {
                        principal
                            .permissions()
                            .iter()
                            .map(|permission| access_permission(*permission))
                            .collect()
                    })
                    .unwrap_or_default(),
                resource_scopes: principal
                    .map(|principal| principal.resource_scopes().iter().map(scope).collect())
                    .unwrap_or_default(),
            },
        }
    }
}

const fn message_class(class: TargetMessageClass) -> &'static str {
    match class {
        TargetMessageClass::StationSnapshot => "station_snapshot",
        TargetMessageClass::DomainEvent => "domain_event",
        TargetMessageClass::CommandResult => "command_result",
        TargetMessageClass::Diagnostic => "diagnostic",
    }
}

const fn delivery_semantic(semantic: DeliverySemantic) -> &'static str {
    match semantic {
        DeliverySemantic::LocalExposure => "local_exposure",
        DeliverySemantic::NamedPeerAcknowledgement => "named_peer_acknowledgement",
        DeliverySemantic::UncertainHandoff => "uncertain_handoff",
    }
}

const fn access_permission(permission: AccessPermission) -> &'static str {
    match permission {
        AccessPermission::Read => "read",
        AccessPermission::Control => "control",
        AccessPermission::PrivilegedControl => "privileged_control",
    }
}

fn scope(scope: &AccessResourceScope) -> ScopeView {
    match scope {
        AccessResourceScope::Bridge(bridge_id) => ScopeView::Bridge {
            bridge_id: bridge_id.as_str().to_owned(),
        },
        AccessResourceScope::Station {
            bridge_id,
            station_id,
        } => ScopeView::Station {
            bridge_id: bridge_id.as_str().to_owned(),
            station_id: station_id.as_str().to_owned(),
        },
        AccessResourceScope::Resource(resource) => ScopeView::Resource {
            bridge_id: resource.bridge_id.as_str().to_owned(),
            station_id: resource.station_id.as_str().to_owned(),
        },
    }
}
