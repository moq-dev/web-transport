use std::io;
use std::sync::Arc;
use std::time::Duration;
use tokio_quiche::settings::{CertificateKind, Hooks, TlsCertificatePaths};

use rustls_pki_types::{CertificateDer, PrivateKeyDer};

use crate::ez::socket::capabilities;
use crate::ez::tls::{ClientHook, ClientVerify};
use crate::ez::DriverState;

use super::{Connection, ConnectionError, Driver, Lock, Settings};

// Local buffer between the application and the driver task — *not* the QUIC
// datagram queue (configured via `Settings::dgram_send_max_queue_len`). It
// only absorbs scheduling latency between `send_datagram()` and the driver
// picking the buffer up, so a small fixed size is sufficient. Anything past
// this is dropped at the channel boundary, which is consistent with the
// unreliable QUIC datagram contract and avoids hiding drops from quiche's
// own (configurable) queue.
pub(super) const DGRAM_CHANNEL_CAPACITY: usize = 64;

/// Construct a QUIC client using sane defaults.
///
/// Unlike [ServerBuilder](super::ServerBuilder), there is no metrics
/// counterpart. `tokio-quiche` hardcodes its own `DefaultMetrics` inside
/// `quic::connect_with_config`, so there is no client-side hook to attach a
/// custom [Metrics](super::Metrics) implementation to.
pub struct ClientBuilder {
    settings: Settings,
    socket: Option<tokio::net::UdpSocket>,
    tls: Option<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)>,
    verify: ClientVerify,
    server_name: Option<String>,
    keep_alive: Option<Duration>,
    gso: bool,
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ClientBuilder {
    /// Create a new client builder.
    pub fn new() -> Self {
        let mut settings = Settings::default();
        settings.verify_peer = true;

        Self {
            settings,
            socket: None,
            tls: None,
            verify: ClientVerify::Default,
            server_name: None,
            keep_alive: None,
            gso: true,
        }
    }

    /// Send a PING on this interval, keeping an idle connection alive.
    ///
    /// Disabled by default. This must be shorter than the peer's
    /// [Settings::max_idle_timeout] to have any effect; a third of it is a
    /// reasonable choice.
    pub fn with_keep_alive(mut self, interval: Duration) -> Self {
        self.keep_alive = Some(interval);
        self
    }

    /// Enable UDP generic segmentation offload (GSO), on by default.
    ///
    /// GSO cuts syscall overhead at high throughput by handing the kernel
    /// several packets at once, but some NICs and virtual network stacks
    /// mishandle it. Turn it off if large sends are being dropped.
    ///
    /// Only Linux supports GSO; elsewhere this does nothing.
    pub fn with_gso(mut self, enabled: bool) -> Self {
        self.gso = enabled;
        self
    }

    /// Listen for incoming packets on the given socket.
    ///
    /// Defaults to an ephemeral port if not specified.
    pub fn with_socket(self, socket: std::net::UdpSocket) -> io::Result<Self> {
        socket.set_nonblocking(true)?;
        let socket = tokio::net::UdpSocket::from_std(socket)?;

        Ok(Self {
            socket: Some(socket),
            ..self
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
            ..self
        }
    }

    /// Use this name for SNI and certificate verification instead of the host
    /// passed to [ClientBuilder::connect].
    ///
    /// The dial target is unchanged; only the name the server certificate must
    /// match is. This is how you reach a host by IP, or through a tunnel, while
    /// still verifying the certificate it was actually issued for.
    pub fn with_server_name(mut self, name: impl Into<String>) -> Self {
        self.server_name = Some(name.into());
        self
    }

    /// Verify the server certificate against an explicit set of root
    /// certificates instead of the system trust store.
    pub fn with_root_certificates(mut self, roots: Vec<CertificateDer<'static>>) -> Self {
        self.verify = ClientVerify::Roots(roots);
        self
    }

    /// Accept the server certificate only if the SHA-256 of its DER encoding
    /// matches one of the provided hashes, bypassing CA verification.
    ///
    /// This mirrors the browser's `serverCertificateHashes` option and is the
    /// usual way to reach a relay using a short-lived self-signed certificate.
    pub fn with_server_certificate_hashes(mut self, hashes: Vec<[u8; 32]>) -> Self {
        self.verify = ClientVerify::Hashes(hashes);
        self
    }

    /// Connect to the QUIC server at the given host and port.
    ///
    /// `host` is the dial target: it's resolved via DNS and, unless
    /// [ClientBuilder::with_server_name] overrides it, is also the name the
    /// server's certificate must match.
    ///
    /// This takes ownership because the underlying quiche implementation doesn't support reusing the same socket.
    pub async fn connect(mut self, host: &str, port: u16) -> io::Result<Connecting> {
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

        // Enable the offloads the kernel supports before the socket is wrapped;
        // `from_udp` starts with everything disabled.
        let capabilities = capabilities(&socket, self.gso);

        // Connect to the server using the addr we just resolved.
        let mut socket = tokio_quiche::socket::Socket::<
            Arc<tokio::net::UdpSocket>,
            Arc<tokio::net::UdpSocket>,
        >::from_udp(socket)?;
        socket.capabilities = capabilities;

        // Only the fully-insecure path (no verification of any kind) deserves a
        // warning; hash- and root-based verification still authenticate the peer.
        if !self.settings.verify_peer && matches!(self.verify, ClientVerify::Default) {
            tracing::warn!("TLS certificate verification is disabled, a MITM attack is possible");
        }

        // Install a TLS hook whenever we present a client certificate or need a
        // non-default verification policy. The SSL context is built (and the
        // certificate material validated) here so a bad cert/key/root fails the
        // connection rather than silently dropping the policy inside the hook.
        // ALPN is left to tokio-quiche, which applies it after the hook runs.
        let needs_hook = self.tls.is_some() || !matches!(self.verify, ClientVerify::Default);
        let (tls_cert, hooks) = if needs_hook {
            let ctx = crate::ez::tls::build_client_context(self.tls.as_ref(), &self.verify)?;
            let hook = ClientHook::new(ctx);
            // ConnectionHook is only invoked when tls_cert is set, so we provide a dummy.
            let dummy_tls = TlsCertificatePaths {
                cert: "",
                private_key: "",
                kind: CertificateKind::X509,
            };
            let hooks = Hooks {
                connection_hook: Some(Arc::new(hook)),
            };
            (Some(dummy_tls), hooks)
        } else {
            (None, Hooks::default())
        };

        // quiche uses this for both SNI and the certificate's hostname check.
        let server_name = self.server_name.as_deref().unwrap_or(host);

        let params = tokio_quiche::ConnectionParams::new_client(self.settings, tls_cert, hooks);

        let accept_bi = flume::unbounded();
        let accept_uni = flume::unbounded();
        let dgram_in = flume::bounded(DGRAM_CHANNEL_CAPACITY);
        let dgram_out = flume::bounded(DGRAM_CHANNEL_CAPACITY);
        let dgram_max = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let driver = Lock::new(DriverState::new(false));
        let app = Driver::new(
            driver.clone(),
            accept_bi.0,
            accept_uni.0,
            dgram_in.0,
            dgram_out.1,
            dgram_max.clone(),
            self.keep_alive,
        );

        let conn = tokio_quiche::quic::connect_with_config(socket, Some(server_name), &params, app)
            .await
            .map_err(|e| io::Error::other(e.to_string()))?;

        let conn = Connection::new(
            conn,
            driver.clone(),
            accept_bi.1,
            accept_uni.1,
            dgram_in.1,
            dgram_out.0,
            dgram_max,
        );
        Ok(Connecting {
            connection: conn,
            driver,
        })
    }
}

/// A QUIC connection that is still completing the TLS handshake.
///
/// This is the client-side equivalent of [super::Incoming] on the server side.
/// Call [Connecting::established] to wait for the handshake to complete.
pub struct Connecting {
    connection: Connection,
    driver: Lock<DriverState>,
}

impl Connecting {
    /// Wait for the TLS handshake to complete.
    ///
    /// Returns the connection once the handshake is complete, or an error if the connection
    /// is closed before the handshake finishes.
    pub async fn established(self) -> Result<Connection, ConnectionError> {
        use std::future::poll_fn;

        poll_fn(|cx| self.driver.lock().poll_handshake(cx.waker())).await?;

        Ok(self.connection)
    }
}
