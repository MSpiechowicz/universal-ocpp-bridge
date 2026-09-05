#![doc = "Crash-safe SQLite implementation of the application-owned operational store."]

mod codec;
mod command;
mod configuration;
mod delivery;
mod lifecycle;
mod recovery;
mod retention;
mod schema;
mod store;
mod worker;

pub use configuration::{MINIMUM_SQLITE_VERSION, SqliteRuntimeConfiguration};
pub use retention::SqliteRetentionPolicy;
pub use store::{DEFAULT_WORK_QUEUE_CAPACITY, SqliteOperationalStore};
