use std::{io, sync::Arc, time::Duration};

use tokio::time::{Instant, Interval};
use uob_application::{Application, DatabaseHealthState, ExportSpool, ExportSpoolNamespace};
use uob_external_export_adapter::{ExportScheduler, ExportSchedulerHandle, ValidatedDataExport};

/// Resources are supplied only after an enabled provider and a durable spool are available.
pub(crate) struct ExportResources {
    pub namespace: ExportSpoolNamespace,
    pub spool: Arc<dyn ExportSpool>,
}

pub(crate) struct ExportRuntime {
    handle: ExportSchedulerHandle,
    interval: Interval,
}

impl ExportRuntime {
    pub(crate) fn start(
        selection: ValidatedDataExport,
        resources: Option<ExportResources>,
        application: &Application,
    ) -> io::Result<Option<Self>> {
        if !selection.is_enabled() {
            return Ok(None);
        }
        let resources = resources.ok_or_else(|| {
            io::Error::other("enabled export requires a durable spool and source namespace")
        })?;
        let handle = ExportScheduler::start(
            selection,
            resources.namespace,
            resources.spool,
            application.health().resources().clone(),
        )
        .map_err(|_| io::Error::other("export scheduler startup failed"))?
        .ok_or_else(|| io::Error::other("enabled exporter did not start"))?;
        let interval = tokio::time::interval(Duration::from_millis(250));
        let runtime = Self { handle, interval };
        runtime.report_health(application);
        Ok(Some(runtime))
    }

    pub(crate) async fn observe(&mut self, application: &Application) {
        self.interval.tick().await;
        self.report_health(application);
    }

    fn report_health(&self, application: &Application) {
        let mut health = self.handle.health().provider;
        if health.state == DatabaseHealthState::Stopped {
            health.state = DatabaseHealthState::Degraded;
        }
        application.health().report_exporter(&health);
    }

    /// Exporter failure or deadline expiry degrades only the optional component.
    pub(crate) async fn shutdown(self, application: &Application, deadline: Instant) {
        let mut health = self.handle.health().provider;
        let result = tokio::time::timeout_at(deadline, self.handle.shutdown()).await;
        if !matches!(result, Ok(Ok(()))) {
            health.state = DatabaseHealthState::Degraded;
            health.reason = Some("export.shutdown_failed".to_owned());
            application.health().report_exporter(&health);
        }
    }
}
