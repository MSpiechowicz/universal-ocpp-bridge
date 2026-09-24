use super::{CliResult, configuration, failure, success};

pub(super) async fn serve(configuration_path: &std::path::Path, no_ui: bool) -> CliResult {
    let configuration = match configuration::load(configuration_path) {
        Ok(configuration) => configuration,
        Err(error) => return failure(2, error.to_string()),
    };
    if let Err(error) = crate::staging_network::verify(
        configuration
            .service
            .application
            .identity()
            .runtime
            .environment,
    ) {
        return failure(1, error.to_owned());
    }
    let deployment = match configuration
        .deployment
        .as_ref()
        .map(|layout| layout.open(configuration.service.application.identity()))
        .transpose()
    {
        Ok(deployment) => deployment,
        Err(error) => return failure(1, error.to_owned()),
    };
    let charging = match configuration.charging {
        Some(config) => match crate::charging::ChargingRuntime::open(
            config,
            &configuration.service.application,
            configuration
                .service
                .target_selection
                .as_ref()
                .map(|selection| {
                    (
                        selection.target_id.clone(),
                        selection.configuration().configuration().revision,
                    )
                }),
        )
        .await
        {
            Ok(runtime) => Some(runtime),
            Err(error) => return failure(1, format!("charging startup {}", error.kind())),
        },
        None => None,
    };
    let options = uob_management_adapter::ManagementRouterOptions {
        static_assets: !no_ui,
    };
    let diagnostics = match configuration.diagnostics.resolve_with_resources(
        configuration
            .service
            .application
            .health()
            .resources()
            .clone(),
    ) {
        Ok(value) => value,
        Err(error) => return failure(1, error.to_string()),
    };
    let release_read = match configuration.release_read.resolve() {
        Ok(value) => value,
        Err(error) => return failure(1, error.to_string()),
    };
    eprintln!(
        "service listening on {} (static assets: {})",
        configuration.management_address,
        if no_ui { "disabled" } else { "enabled" }
    );
    let result = crate::lifecycle::serve(
        crate::diagnostics::instrument(
            configuration.service.application,
            diagnostics.manager.clone(),
        ),
        crate::lifecycle::ServeSettings {
            address: configuration.management_address,
            diagnostics,
            release_read,
            options,
            deadline: configuration.shutdown_timeout,
            deployment,
            charging,
        },
    )
    .await;
    match result {
        Ok(()) => success(),
        Err(error) => failure(1, format!("service runtime {}", error.kind())),
    }
}
