#![doc = "Target adapter extension seam owned outside charging workflows."]

use std::collections::BTreeMap;

/// Minimal composition seam for target implementations.
///
/// The full lifecycle and data contracts are intentionally left to the dedicated
/// target-contract backlog item.
pub trait TargetAdapter: Send + Sync {
    /// Stable target kind registered in the service composition root.
    fn kind(&self) -> &'static str;
}

/// Errors produced while composing target adapters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegistrationError {
    /// An adapter with the same stable kind was already registered.
    DuplicateKind(&'static str),
}

/// Registry populated exclusively by the service composition root.
#[derive(Default)]
pub struct TargetRegistry {
    adapters: BTreeMap<&'static str, Box<dyn TargetAdapter>>,
}

impl TargetRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            adapters: BTreeMap::new(),
        }
    }

    /// Registers a concrete adapter without modifying application workflows.
    ///
    /// # Errors
    ///
    /// Returns [`RegistrationError::DuplicateKind`] when the kind is already
    /// present in this registry.
    pub fn register(
        &mut self,
        adapter: impl TargetAdapter + 'static,
    ) -> Result<(), RegistrationError> {
        let kind = adapter.kind();
        if self.adapters.contains_key(kind) {
            return Err(RegistrationError::DuplicateKind(kind));
        }
        self.adapters.insert(kind, Box::new(adapter));
        Ok(())
    }

    /// Returns whether a target kind is available in this executable.
    #[must_use]
    pub fn contains(&self, kind: &str) -> bool {
        self.adapters.contains_key(kind)
    }

    /// Returns the number of registered target kinds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.adapters.len()
    }

    /// Returns whether no target kinds are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.adapters.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{RegistrationError, TargetAdapter, TargetRegistry};

    struct TestTarget;

    impl TargetAdapter for TestTarget {
        fn kind(&self) -> &'static str {
            "test.memory"
        }
    }

    #[test]
    fn duplicate_kinds_are_rejected() {
        let mut registry = TargetRegistry::new();
        registry.register(TestTarget).expect("first registration");

        assert_eq!(
            registry.register(TestTarget),
            Err(RegistrationError::DuplicateKind("test.memory"))
        );
    }
}
