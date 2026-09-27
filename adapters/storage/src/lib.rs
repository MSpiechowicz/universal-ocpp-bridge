#![doc = "Crash-safe SQLite operational storage and isolated export spool adapters."]

pub mod backup;
mod codec;
mod command;
mod command_history;
mod configuration;
mod delivery;
mod drain;
mod lifecycle;
mod recovery;
mod remote_control;
mod retention;
mod schema;
mod snapshots;
mod spool;
mod store;
mod trigger;
mod trigger201;
mod worker;

pub use configuration::{MINIMUM_SQLITE_VERSION, SqliteRuntimeConfiguration};
pub use retention::SqliteRetentionPolicy;
pub use spool::{Limits as ExportSpoolLimits, SqliteExportSpool};
pub use store::{DEFAULT_WORK_QUEUE_CAPACITY, SqliteOperationalStore};
