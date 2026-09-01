#![doc = "Dependency-light shared contracts for Universal OCPP Bridge."]

/// Identifies the version of an application-owned contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContractVersion {
    /// Major compatibility version.
    pub major: u16,
    /// Additive revision within the major version.
    pub revision: u16,
}

impl ContractVersion {
    /// The initial contract marker used while the canonical schemas are introduced.
    pub const V1_INITIAL: Self = Self {
        major: 1,
        revision: 0,
    };
}

#[cfg(test)]
mod tests {
    use super::ContractVersion;

    #[test]
    fn initial_contract_has_expected_compatibility_major() {
        assert_eq!(ContractVersion::V1_INITIAL.major, 1);
    }
}
