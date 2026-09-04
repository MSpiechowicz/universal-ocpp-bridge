use uob_application::{ConfigurationError, ConfigurationErrorCode};

use super::field;

/// Stable preset selecting the industrial EMS/SCADA contracts on the same MQTT target.
pub const EMS_SCADA_PROFILE: &str = "ems-scada";

/// Stable default preset publishing the canonical bridge namespace only.
pub const STANDARD_PROFILE: &str = "standard";

/// Catalog preset selected inside one MQTT target; a preset is never a second target.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum MqttProfile {
    /// Canonical bridge namespace without an industrial point catalog.
    #[default]
    Standard,
    /// Industrial preset publishing retained canonical point descriptors and latest values.
    EmsScada,
}

impl MqttProfile {
    pub(super) fn parse(value: &str) -> Result<Self, ConfigurationError> {
        match value {
            STANDARD_PROFILE => Ok(Self::Standard),
            EMS_SCADA_PROFILE => Ok(Self::EmsScada),
            _ => Err(field(ConfigurationErrorCode::Unsupported, "profile")),
        }
    }

    pub(crate) const fn publishes_point_catalog(self) -> bool {
        matches!(self, Self::EmsScada)
    }
}
