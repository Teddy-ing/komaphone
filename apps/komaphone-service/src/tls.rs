//! Bounded native TLS listener. Private certificates use normal certificate verification.
use crate::{Error, Result};
use axum::serve::Listener;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    net::{TcpListener, TcpStream},
    task::JoinSet,
};
use tokio_rustls::{
    TlsAcceptor,
    rustls::{
        self,
        pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
    },
    server::TlsStream,
};

pub const MAX_PEM_BYTES: usize = 64 * 1024;
const MAX_HANDSHAKES: usize = 32;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone)]
pub struct TlsConfig(Arc<rustls::ServerConfig>);

impl TlsConfig {
    pub fn from_pem(certificate: &[u8], private_key: &[u8]) -> Result<Self> {
        if certificate.len() > MAX_PEM_BYTES || private_key.len() > MAX_PEM_BYTES {
            return Err(Error::Invalid("TLS certificate/key exceeds 64 KiB"));
        }
        let certificates = CertificateDer::pem_slice_iter(certificate)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| Error::Invalid("invalid TLS certificate PEM"))?;
        if certificates.is_empty() || certificates.len() > 8 {
            return Err(Error::Invalid("TLS certificate chain must contain 1 to 8 certificates"));
        }
        let key = PrivateKeyDer::from_pem_slice(private_key).map_err(|_| Error::Invalid("invalid TLS private key PEM"))?;
        // Explicit provider avoids process-global selection and its panic path when
        // other editor dependencies enable an additional rustls provider.
        let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|_| Error::Invalid("TLS protocol configuration"))?
            .with_no_client_auth()
            .with_single_cert(certificates, key)
            .map_err(|_| Error::Invalid("TLS certificate/private key mismatch or unsupported key"))?;
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Self(Arc::new(config)))
    }
}

pub(crate) struct TlsListener {
    tcp: TcpListener,
    acceptor: TlsAcceptor,
    handshakes: JoinSet<Option<(TlsStream<TcpStream>, SocketAddr)>>,
}

impl TlsListener {
    pub(crate) fn new(tcp: TcpListener, config: TlsConfig) -> Self {
        Self { tcp, acceptor: TlsAcceptor::from(config.0), handshakes: JoinSet::new() }
    }
}

impl Listener for TlsListener {
    type Io = TlsStream<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            tokio::select! {
                result = self.handshakes.join_next(), if !self.handshakes.is_empty() => {
                    if let Some(Ok(Some(connection))) = result { return connection; }
                }
                result = self.tcp.accept(), if self.handshakes.len() < MAX_HANDSHAKES => {
                    match result {
                        Ok((stream, address)) => {
                            let acceptor = self.acceptor.clone();
                            self.handshakes.spawn(async move {
                                tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await.ok()?.ok().map(|stream| (stream, address))
                            });
                        }
                        Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
                    }
                }
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.tcp.local_addr()
    }
}
