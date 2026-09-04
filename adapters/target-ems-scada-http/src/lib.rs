#![doc = "Direct EMS/SCADA HTTP integration target."]
#![doc = ""]
#![doc = "The adapter serves one scoped integration listener under `/bridge/v1`. Administrative"]
#![doc = "configuration, debug capture, and simulator controls stay on the independent management"]
#![doc = "API, and canonical reads and command admission reuse the shared application ports rather"]
#![doc = "than duplicating business handlers or storage."]

mod capabilities;
mod configuration;
mod error;
mod routing;
mod session;
mod target;

pub use configuration::{
    EMS_SCADA_HTTP_TARGET_KIND, EmsScadaHttpRuntimeOptions, EmsScadaHttpTargetFactory,
    INTEGRATION_PATH_PREFIX, IntegrationCredentials, IntegrationPrincipal,
    ems_scada_http_configuration_schema,
};
pub use error::IntegrationErrorCode;
