use std::{
    error::Error as _,
    io,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
    task::JoinHandle,
    time::{Instant, timeout},
};
use tokio_postgres::{Client, Config, NoTls, config::SslMode};
use uob_application::DatabaseError;

use crate::{
    credentials::resolve,
    errors::{invalid, shutdown, unavailable, uncertain},
    inbound::{Guarded, GuardedTls},
    settings::Settings,
    tls,
};

const CONNECT_LIMIT: Duration = Duration::from_secs(5);
const QUERY_LIMIT: Duration = Duration::from_secs(2);
const STOP_LIMIT: Duration = Duration::from_secs(1);
static DRIVER_TASKS: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn driver_tasks() -> usize {
    DRIVER_TASKS.load(Ordering::SeqCst)
}

struct TaskGuard;
impl Drop for TaskGuard {
    fn drop(&mut self) {
        DRIVER_TASKS.fetch_sub(1, Ordering::SeqCst);
    }
}

pub(crate) struct Connection {
    client: Client,
    driver: Option<JoinHandle<()>>,
}

impl Connection {
    pub async fn connect(settings: &Settings) -> Result<Self, DatabaseError> {
        let deadline = Instant::now() + CONNECT_LIMIT;
        let credentials = resolve(settings)?;
        let mut config = Config::new();
        config
            .user(&credentials.username)
            .password(credentials.password)
            .dbname(&settings.database)
            .host(&settings.hostname)
            .hostaddr(settings.endpoint.ip())
            .port(settings.endpoint.port())
            .application_name("uob_postgresql_qualification")
            .connect_timeout(CONNECT_LIMIT)
            .keepalives(false)
            .ssl_mode(if settings.tls {
                SslMode::Require
            } else {
                SslMode::Disable
            });
        if settings.tls {
            let ca = credentials
                .ca
                .as_deref()
                .ok_or_else(|| invalid("postgres.ca.invalid"))?;
            let connector = GuardedTls(tls::connector(ca)?);
            let (client, connection) = timeout_at(deadline, config.connect(connector))
                .await
                .map_err(|_| unavailable("postgres.connect.deadline"))?
                .map_err(|error| connect_error(&error))?;
            return Self::start(client, connection, deadline).await;
        }
        let socket = timeout_at(deadline, TcpStream::connect(settings.endpoint))
            .await
            .map_err(|_| unavailable("postgres.connect.deadline"))?
            .map_err(|_| unavailable("postgres.connect.unavailable"))?;
        let (client, connection) =
            timeout_at(deadline, config.connect_raw(Guarded::new(socket), NoTls))
                .await
                .map_err(|_| unavailable("postgres.connect.deadline"))?
                .map_err(|error| connect_error(&error))?;
        Self::start(client, connection, deadline).await
    }

    async fn start<S, T>(
        client: Client,
        connection: tokio_postgres::Connection<S, T>,
        deadline: Instant,
    ) -> Result<Self, DatabaseError>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
        T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        DRIVER_TASKS.fetch_add(1, Ordering::SeqCst);
        let guard = TaskGuard;
        let driver = tokio::spawn(async move {
            let _guard = guard;
            let _ = connection.await;
        });
        let mut session = Self {
            client,
            driver: Some(driver),
        };
        let result = timeout_at(
            deadline,
            session
                .client
                .batch_execute("SET statement_timeout = '1500ms'"),
        )
        .await;
        if let Ok(Ok(())) = result {
            Ok(session)
        } else {
            let _ = session.close().await;
            Err(unavailable("postgres.connect.setup"))
        }
    }

    pub async fn probe(&mut self) -> Result<(), DatabaseError> {
        let result = timeout(QUERY_LIMIT, self.client.query_one("SELECT 1::INT4", &[])).await;
        match result {
            Ok(Ok(row)) if row.get::<_, i32>(0) == 1 => Ok(()),
            _ => {
                self.close().await?;
                Err(unavailable("postgres.probe.failed"))
            }
        }
    }

    pub async fn sleep(&mut self) -> Result<(), DatabaseError> {
        let result = timeout(
            QUERY_LIMIT,
            self.client.query_one("SELECT pg_sleep(10)", &[]),
        )
        .await;
        if let Ok(Ok(_)) = result {
            Err(invalid("postgres.sleep.unexpected"))
        } else {
            self.close().await?;
            Err(unavailable("postgres.probe.deadline"))
        }
    }

    pub async fn transaction(&mut self, marker: &str) -> Result<(), DatabaseError> {
        let result = timeout(QUERY_LIMIT, self.transaction_inner(marker)).await;
        match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => {
                self.close().await?;
                Err(error)
            }
            Err(_) => {
                self.close().await?;
                Err(uncertain("postgres.transaction.deadline"))
            }
        }
    }

    async fn transaction_inner(&mut self, marker: &str) -> Result<(), DatabaseError> {
        let transaction = self
            .client
            .transaction()
            .await
            .map_err(|_| unavailable("postgres.transaction.begin"))?;
        transaction
            .execute(
                "INSERT INTO public.qualification_markers (marker) VALUES ($1)",
                &[&marker],
            )
            .await
            .map_err(|_| uncertain("postgres.transaction.insert"))?;
        transaction
            .commit()
            .await
            .map_err(|_| uncertain("postgres.transaction.commit"))
    }

    pub async fn close(&mut self) -> Result<(), DatabaseError> {
        if let Some(mut driver) = self.driver.take() {
            driver.abort();
            let joined = timeout(STOP_LIMIT, &mut driver)
                .await
                .map_err(|_| shutdown())?;
            if matches!(joined, Err(error) if !error.is_cancelled()) {
                return Err(shutdown());
            }
        }
        Ok(())
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if let Some(driver) = &self.driver {
            driver.abort();
        }
    }
}

async fn timeout_at<F: Future>(
    deadline: Instant,
    future: F,
) -> Result<F::Output, tokio::time::error::Elapsed> {
    tokio::time::timeout_at(deadline, future).await
}

fn connect_error(error: &tokio_postgres::Error) -> DatabaseError {
    if error.is_closed() {
        return unavailable("postgres.connect.unavailable");
    }

    if error
        .source()
        .and_then(|source| source.downcast_ref::<io::Error>())
        .is_some_and(|source| {
            matches!(
                source.kind(),
                io::ErrorKind::ConnectionRefused
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::TimedOut
            )
        })
    {
        return unavailable("postgres.connect.unavailable");
    }
    // Authentication, TLS trust and unknown handshake errors require intervention.
    // Classify unknown errors conservatively instead of inspecting secret-bearing text.
    invalid("postgres.connect.rejected")
}
