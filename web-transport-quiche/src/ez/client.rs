use std::io;
use std::sync::Arc;
use tokio_quiche::settings::{CertificateKind, Hooks, TlsCertificatePaths};

use rustls_pki_types::{CertificateDer, PrivateKeyDer};

use crate::ez::tls::StaticCertHook;
use crate::ez::DriverState;

use super::{Connection, DefaultMetrics, Driver, Lock, Metrics, Settings};

/// Construct a QUIC client using sane defaults.
pub struct ClientBuilder<M: Metrics = DefaultMetrics> {
    settings: Settings,
    socket: Option<tokio::net::UdpSocket>,
    tls: Option<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)>,
    metrics: M,
}

impl Default for ClientBuilder<DefaultMetrics> {
    fn default() -> Self {
        Self::with_metrics(DefaultMetrics)
    }
}

impl<M: Metrics> ClientBuilder<M> {
    /// Create a new client builder with custom metrics.
    pub fn with_metrics(m: M) -> Self {
        let mut settings = Settings::default();
        settings.verify_peer = true;

        Self {
            settings,
            metrics: m,
            socket: None,
            tls: None,
        }
    }

    /// Listen for incoming packets on the given socket.
    ///
    /// Defaults to an ephemeral port if not specified.
    pub fn with_socket(self, socket: std::net::UdpSocket) -> io::Result<Self> {
        socket.set_nonblocking(true)?;
        let socket = tokio::net::UdpSocket::from_std(socket)?;

        /*
        // TODO Modify quiche to add other platform support.
        #[cfg(target_os = "linux")]
        let capabilities = SocketCapabilities::apply_all_and_get_compatibility(&socket);
        #[cfg(not(target_os = "linux"))]
        let capabilities = SocketCapabilities::default();
        */

        Ok(Self {
            socket: Some(socket),
            settings: self.settings,
            metrics: self.metrics,
            tls: self.tls,
        })
    }

    /// Listen for incoming packets on the given address.
    ///
    /// Defaults to an ephemeral port if not specified.
    pub fn with_bind<A: std::net::ToSocketAddrs>(self, addrs: A) -> io::Result<Self> {
        // We use std to avoid async
        let socket = std::net::UdpSocket::bind(addrs)?;
        self.with_socket(socket)
    }

    /// Use the provided [Settings] instead of the defaults.
    ///
    /// WARNING: [Settings::verify_peer] is set to false by default.
    /// This will completely bypass certificate verification and is generally not recommended.
    pub fn with_settings(mut self, settings: Settings) -> Self {
        self.settings = settings;
        self
    }

    /// Optional: Use a client certificate for mTLS.
    pub fn with_single_cert(
        self,
        chain: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
    ) -> Self {
        Self {
            tls: Some((chain, key)),
            settings: self.settings,
            metrics: self.metrics,
            socket: self.socket,
        }
    }

    /// Connect to the QUIC server at the given host and port.
    ///
    /// This takes ownership because the underlying quiche implementation doesn't support reusing the same socket.
    pub async fn connect(mut self, host: &str, port: u16) -> io::Result<Connection> {
        if self.socket.is_none() {
            self = self.with_bind("[::]:0")?;
        }

        let socket = self.socket.take().unwrap();

        let mut remotes = match tokio::net::lookup_host((host, port)).await {
            Ok(remotes) => remotes,
            Err(err) => {
                return Err(io::Error::new(
                    io::ErrorKind::HostUnreachable,
                    err.to_string(),
                ));
            }
        };

        // Return the first entry.
        let remote = match remotes.next() {
            Some(remote) => remote,
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::HostUnreachable,
                    "no addresses found for host",
                ))
            }
        };

        socket.connect(remote).await?;

        // Connect to the server using the addr we just resolved.
        let socket = tokio_quiche::socket::Socket::<
            Arc<tokio::net::UdpSocket>,
            Arc<tokio::net::UdpSocket>,
        >::from_udp(socket)?;

        if !self.settings.verify_peer {
            tracing::warn!("TLS certificate verification is disabled, a MITM attack is possible");
        }

        let (tls_cert, hooks) = match self.tls {
            Some((chain, key)) => {
                let hook = StaticCertHook {
                    chain,
                    key,
                    alpn: Vec::new(),
                };
                let dummy_tls = TlsCertificatePaths {
                    cert: "",
                    private_key: "",
                    kind: CertificateKind::X509,
                };
                let hooks = Hooks {
                    connection_hook: Some(Arc::new(hook)),
                };
                (Some(dummy_tls), hooks)
            }
            None => (None, Hooks::default()),
        };

        let params = tokio_quiche::ConnectionParams::new_client(self.settings, tls_cert, hooks);

        let accept_bi = flume::unbounded();
        let accept_uni = flume::unbounded();

        let driver = Lock::new(DriverState::new(false));
        let app = Driver::new(driver.clone(), accept_bi.0, accept_uni.0);

        let conn = tokio_quiche::quic::connect_with_config(socket, Some(host), &params, app)
            .await
            .map_err(|e| io::Error::other(e.to_string()))?;

        let conn = Connection::new(conn, driver, accept_bi.1, accept_uni.1);
        Ok(conn)
    }
}
