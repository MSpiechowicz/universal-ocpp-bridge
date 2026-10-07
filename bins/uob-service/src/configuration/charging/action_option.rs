//! One operator-facing boolean TOML key that enables a single privileged station action.
use serde::Deserialize;

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(from = "bool")]
pub(crate) enum StationActionOption {
    #[default]
    Disabled,
    Enabled,
}

impl From<bool> for StationActionOption {
    fn from(enabled: bool) -> Self {
        if enabled {
            Self::Enabled
        } else {
            Self::Disabled
        }
    }
}

impl StationActionOption {
    pub fn enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}
