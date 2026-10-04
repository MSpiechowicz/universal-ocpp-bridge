use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ocpp_client::{
    ClientConfig, ReconnectPolicy, Reconnector, TokioExecutor, TokioTimer, TransportError,
    TransportSink, TransportStream,
};
use tokio::sync::mpsc;

use super::TriggerBarrier;
use crate::trigger::TriggerJob;
use crate::trigger201::TriggerJob201;
use crate::{Ocpp16State, Ocpp201State};

struct TriggerReconnector {
    endpoint: String,
    credentials_file: Option<String>,
    barrier: TriggerBarrier,
}

impl Reconnector for TriggerReconnector {
    fn connect<'a>(
        &'a self,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<
                        (Box<dyn TransportSink>, Box<dyn TransportStream>),
                        TransportError,
                    >,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let (sink, stream) = crate::station_auth::connect(
                &self.endpoint,
                self.barrier.version,
                self.credentials_file.as_deref(),
            )
            .await?;
            let (sink, stream) = if let Some(state) = &self.barrier.state {
                crate::local_authorization::transport::wrap(sink, stream, Arc::clone(state))
            } else {
                (sink, stream)
            };
            Ok(self.barrier.wrap(sink, stream))
        })
    }
}

pub(crate) async fn connect(
    endpoint: &str,
    credentials_file: Option<&str>,
    timeout: Duration,
    reconnect: bool,
    capacity: usize,
    state: Arc<Mutex<Ocpp16State>>,
) -> Result<
    (
        ocpp_client::ocpp_1_6::OCPP1_6Client,
        TriggerBarrier,
        mpsc::UnboundedReceiver<TriggerJob>,
    ),
    TransportError,
> {
    let (delivered, receiver) = mpsc::unbounded_channel();
    let barrier = TriggerBarrier::new(capacity, delivered, Arc::clone(&state), timeout);
    let (sink, stream) =
        crate::station_auth::connect(endpoint, ocpp_client::OcppVersion::V1_6, credentials_file)
            .await?;
    let (sink, stream) = crate::local_authorization::transport::wrap(sink, stream, state);
    let (sink, stream) = barrier.wrap(sink, stream);
    let mut config = ClientConfig::new(timeout);
    if reconnect {
        config = config.with_reconnect(
            Box::new(TriggerReconnector {
                endpoint: endpoint.to_owned(),
                credentials_file: credentials_file.map(str::to_owned),
                barrier: barrier.clone(),
            }),
            ReconnectPolicy::default(),
        );
    }
    let client = ocpp_client::Client::from_transport_with_config(
        sink,
        stream,
        Box::new(TokioExecutor),
        Box::new(TokioTimer),
        config,
    );
    Ok((client, barrier, receiver))
}

pub(crate) async fn connect_201(
    endpoint: &str,
    credentials_file: Option<&str>,
    timeout: Duration,
    reconnect: bool,
    capacity: usize,
    state: Arc<Mutex<Ocpp201State>>,
) -> Result<
    (
        ocpp_client::ocpp_2_0_1::OCPP2_0_1Client,
        TriggerBarrier,
        mpsc::UnboundedReceiver<TriggerJob201>,
    ),
    TransportError,
> {
    let (delivered, receiver) = mpsc::unbounded_channel();
    let barrier = TriggerBarrier::new_201(capacity, delivered, state, timeout);
    let (sink, stream) =
        crate::station_auth::connect(endpoint, barrier.version, credentials_file).await?;
    let (sink, stream) = barrier.wrap(sink, stream);
    let mut config = ClientConfig::new(timeout);
    if reconnect {
        config = config.with_reconnect(
            Box::new(TriggerReconnector {
                endpoint: endpoint.to_owned(),
                credentials_file: credentials_file.map(str::to_owned),
                barrier: barrier.clone(),
            }),
            ReconnectPolicy::default(),
        );
    }
    let client = ocpp_client::Client::from_transport_with_config(
        sink,
        stream,
        Box::new(TokioExecutor),
        Box::new(TokioTimer),
        config,
    );
    Ok((client, barrier, receiver))
}
