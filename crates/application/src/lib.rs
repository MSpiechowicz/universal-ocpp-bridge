#![doc = "Target-neutral application coordination for Universal OCPP Bridge."]

use uob_contracts::ContractVersion;

/// Composition-independent application facade.
#[derive(Clone, Debug, Default)]
pub struct Application;

impl Application {
    /// Creates the application facade without selecting any concrete adapter.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Reports the contract version supported by the domain layer.
    #[must_use]
    pub const fn contract_version(&self) -> ContractVersion {
        uob_domain::supported_contract()
    }
}

#[cfg(test)]
mod tests {
    use super::Application;

    #[test]
    fn application_does_not_require_an_adapter_to_construct() {
        assert_eq!(Application::new().contract_version().major, 1);
    }
}
