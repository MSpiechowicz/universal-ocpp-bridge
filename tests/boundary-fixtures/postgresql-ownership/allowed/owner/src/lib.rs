use tokio_postgres::Client;
pub use postgres_rustls::Connector;

pub fn accepted_client(_: Option<Client>) {}
