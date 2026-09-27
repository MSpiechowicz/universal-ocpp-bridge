/// PostgreSQL configuration is catalog data, not a client import.
pub const KIND: &str = "postgresql";
pub const EXAMPLE: &str = r#"use tokio_postgres::Client;"#;
// pub use postgres_rustls::Connector;
pub use uob_postgresql_export_adapter::accepted_client;
