#![doc = "Production service composition root."]

use uob_application::Application;
use uob_target_adapter::TargetRegistry;

/// Fully composed service dependencies.
pub struct ServiceComposition<E, P> {
    /// Target kinds available in this service build.
    pub targets: TargetRegistry<E, P>,
    /// Target-neutral application facade.
    pub application: Application,
}

/// Creates the production composition without requiring a built-in target.
#[must_use]
pub fn compose<E, P>(targets: TargetRegistry<E, P>) -> ServiceComposition<E, P> {
    ServiceComposition {
        targets,
        application: Application::new(),
    }
}

#[cfg(test)]
mod tests {
    use uob_target_adapter::TargetRegistry;

    use super::compose;

    #[test]
    fn service_starts_without_constructing_an_unselected_target() {
        let service = compose(TargetRegistry::<(), ()>::new());

        assert!(service.targets.is_empty());
        assert_eq!(service.application.contract_version().major, 1);
    }
}
