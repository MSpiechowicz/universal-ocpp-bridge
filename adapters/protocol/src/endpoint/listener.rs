use std::{io, time::Duration};

use axum::{
    extract::connect_info::Connected,
    serve::{IncomingStream, Listener},
};
use rustls::pki_types::CertificateDer;
use tokio::{net::TcpListener, time::sleep};
use tokio_rustls::server::TlsStream;

use crate::StationTlsAcceptor;

/// Certificate evidence collected before HTTP and WebSocket extraction.
#[derive(Clone, Debug, Default)]
pub(super) struct PeerIdentity {
    pub(super) certificates: Vec<CertificateDer<'static>>,
}

pub(super) struct TlsListener {
    tcp: TcpListener,
    acceptor: StationTlsAcceptor,
}

impl TlsListener {
    pub(super) const fn new(tcp: TcpListener, acceptor: StationTlsAcceptor) -> Self {
        Self { tcp, acceptor }
    }
}

impl Listener for TlsListener {
    type Io = TlsStream<tokio::net::TcpStream>;
    type Addr = PeerIdentity;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let Ok((tcp, _)) = self.tcp.accept().await else {
                sleep(Duration::from_secs(1)).await;
                continue;
            };
            let Ok(tls) = self.acceptor.accept(tcp).await else {
                continue;
            };
            let certificates = tls
                .get_ref()
                .1
                .peer_certificates()
                .unwrap_or_default()
                .to_vec();
            return (tls, PeerIdentity { certificates });
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.tcp.local_addr().map(|_| PeerIdentity::default())
    }
}

impl Connected<IncomingStream<'_, TlsListener>> for PeerIdentity {
    fn connect_info(stream: IncomingStream<'_, TlsListener>) -> Self {
        stream.remote_addr().clone()
    }
}

impl Connected<IncomingStream<'_, TcpListener>> for PeerIdentity {
    fn connect_info(_stream: IncomingStream<'_, TcpListener>) -> Self {
        Self::default()
    }
}
