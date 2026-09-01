#![doc = "Production service composition root."]

use uob_application::Application;
use uob_target_adapter::TargetRegistry;

/// Fully composed service dependencies.
pub struct ServiceComposition {
    /// Target kinds available in this service build.
    pub targets: TargetRegistry,
    /// Target-neutral application facade.
    pub application: Application,
}

/// Creates the production composition without requiring a built-in target.
#[must_use]
pub fn compose(targets: TargetRegistry) -> ServiceComposition {
    ServiceComposition {
        targets,
        application: Application::new(),
    }
}

#[cfg(test)]
mod tests {
    use uob_target_adapter::{TargetAdapter, TargetRegistry};

    use super::compose;

    struct AlternateTarget;

    impl TargetAdapter for AlternateTarget {
        fn kind(&self) -> &'static str {
            "example.alternate"
        }
    }

    #[test]
    fn alternate_adapter_is_added_only_in_the_composition_root() {
        let mut targets = TargetRegistry::new();
        targets
            .register(AlternateTarget)
            .expect("unique target kind");

        let service = compose(targets);

        assert!(service.targets.contains("example.alternate"));
        assert_eq!(service.application.contract_version().major, 1);
    }
}
