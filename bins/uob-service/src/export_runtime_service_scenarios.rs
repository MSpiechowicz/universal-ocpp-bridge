use std::{
    fs,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use tokio::{sync::oneshot, task::JoinHandle};

use super::{Fixture, ocpp_charging_session, pending_export, selected};
use crate::{
    configuration,
    export_runtime::{ExportResources, ExportRuntime},
    lifecycle::{self, ServeSettings},
};

struct RunningService {
    observed: uob_application::Application,
    attempts: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
    stop: oneshot::Sender<()>,
    task: JoinHandle<std::io::Result<()>>,
}

#[tokio::test]
async fn enabled_exporter_outage_or_hang_never_blocks_charging_or_local_readiness() {
    for hung in [false, true] {
        let Some(fixture) = Fixture::new() else {
            return;
        };
        let service = start_service(&fixture, hung).await;
        verify_charging_during_outage(&fixture, &service, hung).await;
        verify_shutdown(service, hung).await;
    }
}

async fn start_service(fixture: &Fixture, hung: bool) -> RunningService {
    let configuration =
        configuration::load(&fixture.document).expect("real charging configuration");
    let application = configuration.service.application;
    application
        .health()
        .report_core_loop(uob_application::CoreLoopState::Ready);
    application
        .health()
        .report_storage(uob_application::StorageHealthState::Safe, None);
    let attempts = Arc::new(AtomicUsize::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let (spool, namespace) = pending_export(fixture, &application).await;
    let exporter = ExportRuntime::start(
        selected(attempts.clone(), active.clone(), hung),
        Some(ExportResources { namespace, spool }),
        &application,
    )
    .unwrap();
    let charging =
        crate::charging::ChargingRuntime::open(configuration.charging.unwrap(), &application, None)
            .await
            .unwrap();
    let diagnostics = configuration
        .diagnostics
        .resolve_with_resources(application.health().resources().clone())
        .unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    let observed = application.clone();
    let task = tokio::spawn(lifecycle::serve_until(
        application,
        ServeSettings {
            address: format!("127.0.0.1:{}", fixture.management).parse().unwrap(),
            diagnostics,
            release_read: None,
            options: uob_management_adapter::ManagementRouterOptions {
                static_assets: false,
            },
            deadline: Duration::from_secs(2),
            deployment: None,
            charging: Some(charging),
            target_selection: None,
            exporter,
        },
        async move {
            let _ = stopped.await;
        },
    ));
    RunningService {
        observed,
        attempts,
        active,
        stop,
        task,
    }
}

async fn verify_charging_during_outage(fixture: &Fixture, service: &RunningService, hung: bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while service.attempts.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("provider session started");
    if hung {
        assert_eq!(service.active.load(Ordering::SeqCst), 1);
    }
    ocpp_charging_session(fixture.charging, "before", 0).await;
    tokio::time::timeout(Duration::from_secs(7), async {
        loop {
            let snapshot = service.observed.health().snapshot();
            if matches!(
                snapshot.components.get(&uob_application::ComponentKind::ExternalExporter),
                Some(health) if matches!(
                    health.state,
                    uob_application::ComponentHealthState::Degraded
                        | uob_application::ComponentHealthState::Reconnecting
                ),
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("provider outage reported without stopping service");
    if hung {
        super::command::trigger_while_exporter_hung(fixture).await;
        assert_eq!(service.active.load(Ordering::SeqCst), 1);
    }
    ocpp_charging_session(fixture.charging, "during", 20).await;
    assert_eq!(
        service.observed.health().snapshot().readiness,
        uob_application::ReadinessState::Ready
    );
}

async fn verify_shutdown(service: RunningService, hung: bool) {
    let _ = service.stop.send(());
    tokio::time::timeout(Duration::from_secs(4), service.task)
        .await
        .expect("bounded service shutdown")
        .unwrap()
        .unwrap();
    if hung {
        let exporter = &service.observed.health().snapshot().components
            [&uob_application::ComponentKind::ExternalExporter];
        assert_eq!(
            exporter.state,
            uob_application::ComponentHealthState::Degraded
        );
        assert_eq!(exporter.reason.as_deref(), Some("export.shutdown_failed"));
        tokio::time::timeout(Duration::from_secs(1), async {
            while service.active.load(Ordering::SeqCst) != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("hung provider task aborted on shutdown");
    }
}

#[test]
fn production_startup_keeps_disabled_export_and_rejects_enabled_export() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let disabled = configuration::load(&fixture.document).expect("disabled export accepted");
    assert!(!disabled.service.data_export.is_enabled());
    assert_eq!(
        disabled
            .service
            .application
            .health()
            .snapshot()
            .components
            .get(&uob_application::ComponentKind::ExternalExporter)
            .map(|health| health.state),
        Some(uob_application::ComponentHealthState::Disabled),
    );

    let document = fs::read_to_string(&fixture.document).unwrap();
    fs::write(
        &fixture.document,
        format!("{document}\n[data_export]\nenabled=true\n"),
    )
    .unwrap();
    assert!(matches!(
        configuration::load(&fixture.document),
        Err(configuration::ConfigurationLoadError::UnavailableDataExport)
    ));
}

#[tokio::test]
async fn enabled_selection_cannot_start_without_durable_spool() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let configuration = configuration::load(&fixture.document).unwrap();
    let application = &configuration.service.application;
    assert!(
        ExportRuntime::start(
            selected(
                Arc::new(AtomicUsize::new(0)),
                Arc::new(AtomicUsize::new(0)),
                true
            ),
            None,
            application
        )
        .is_err()
    );
    assert!(
        ExportRuntime::start(configuration.service.data_export, None, application)
            .unwrap()
            .is_none()
    );
}
