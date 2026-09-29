use super::{ManagementSource, outside_scope, require_station, storage_error};
use serde_json::Value;
use uob_application::{
    CommandHistoryQuery, OperationalStore, TargetPortError, TargetQueryAuthorization,
    TargetQueryResult,
};
use uob_contracts::RequestId;

impl ManagementSource {
    pub(super) async fn query_command_history(
        &self,
        authorization: &TargetQueryAuthorization,
        query: CommandHistoryQuery,
    ) -> Result<TargetQueryResult<Value>, TargetPortError> {
        require_station(&query.station)?;
        let scope = authorization.command_history_scope(&query.station);
        if scope.is_empty() {
            return Err(outside_scope());
        }
        let page = self
            .store
            .read_command_history(query.clone(), scope.clone())
            .await
            .map_err(|error| storage_error(&error))?;
        if page.items.len() > usize::from(query.limit.get())
            || page
                .items
                .iter()
                .any(|item| !scope.permits(&item.resource, &query.station))
        {
            return Err(outside_scope());
        }
        Ok(TargetQueryResult::CommandHistory(page))
    }

    pub(super) async fn query_command_result(
        &self,
        authorization: &TargetQueryAuthorization,
        request_id: RequestId,
    ) -> Result<TargetQueryResult<Value>, TargetPortError> {
        let result = self
            .store
            .command_result_by_request_id(request_id)
            .await
            .map_err(|error| storage_error(&error))?;
        if result
            .as_ref()
            .is_some_and(|result| !authorization.permits_resource(&result.resource))
        {
            return Err(outside_scope());
        }
        Ok(TargetQueryResult::CommandResult(result))
    }
}
