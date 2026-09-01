#![doc = "Pure charging-domain rules and state."]

use uob_contracts::ContractVersion;

/// Describes the compatibility level understood by the charging domain.
#[must_use]
pub const fn supported_contract() -> ContractVersion {
    ContractVersion::V1_INITIAL
}
