use super::*;

pub(super) struct ReadAuthenticator;

impl ManagementEventAuthenticator for ReadAuthenticator {
    fn authenticate(&self, token: &str) -> Option<AuthenticatedEventAccess> {
        (token == "reader-secret").then(|| AuthenticatedEventAccess {
            authorization: TargetQueryAuthorization::new(
                TargetInstanceId::new("management-read").unwrap(),
                vec![
                    TargetQueryPermission::StationSnapshots,
                    TargetQueryPermission::CommandStatus,
                    TargetQueryPermission::RetainedEvents,
                ],
                vec![TargetResourceScope::Station {
                    bridge_id: resource().bridge_id,
                    station_id: resource().station_id,
                }],
            ),
            default_resource: resource(),
        })
    }
}
