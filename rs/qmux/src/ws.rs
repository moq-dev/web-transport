use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::tungstenite;

use crate::protocol::validate_protocol;
use crate::transport::WsTransport;
use crate::{alpn, Config, Error, Session, Version};

/// Keep-alive configuration for WebSocket transports.
///
/// WebSocket has no built-in idle timeout: when the peer's host crashes
/// or its network drops without sending a TCP FIN, the local socket
/// stays "open" until OS-level TCP keep_alive eventually probes — typically
/// hours. Set this to send periodic Pings and close the session if no
/// frame arrives within `timeout`.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct KeepAlive {
    /// How often to send a Ping frame to the peer.
    pub interval: Duration,

    /// Close the session if no frame is received from the peer within this window.
    /// Should be a small multiple of `interval` to tolerate transient drops.
    pub timeout: Duration,
}

impl KeepAlive {
    /// Create a keep-alive config with the given interval and timeout.
    pub fn new(interval: Duration, timeout: Duration) -> Self {
        Self { interval, timeout }
    }
}

impl Default for KeepAlive {
    fn default() -> Self {
        // Match the QUIC defaults used by moq-native: 5s ping, 30s deadline.
        Self {
            interval: Duration::from_secs(5),
            timeout: Duration::from_secs(30),
        }
    }
}

/// Wrap an already-upgraded WebSocket as a QMux session.
///
/// Use this when the WebSocket handshake was performed by an external
/// framework (e.g. axum) and the caller already has the negotiated
/// `Sec-WebSocket-Protocol` value. Pass it via [`Upgraded::with_alpn`] so the
/// QMux wire-format version and application protocol can be recovered.
/// Without an alpn the legacy `webtransport` wire format is used.
pub struct Upgraded<T> {
    ws: T,
    alpn: Option<String>,
    keep_alive: Option<KeepAlive>,
}

impl<T> Upgraded<T>
where
    T: futures::Stream<Item = Result<tungstenite::Message, tungstenite::Error>>
        + futures::Sink<tungstenite::Message, Error = tungstenite::Error>
        + Unpin
        + Send
        + 'static,
{
    pub fn new(ws: T) -> Self {
        Self {
            ws,
            alpn: None,
            keep_alive: None,
        }
    }

    /// Set the negotiated `Sec-WebSocket-Protocol` value from the handshake.
    pub fn with_alpn(mut self, alpn: &str) -> Self {
        self.alpn = Some(alpn.to_string());
        self
    }

    /// Drive a keep-alive Ping/timeout on the WebSocket.
    pub fn with_keep_alive(mut self, keep_alive: KeepAlive) -> Self {
        self.keep_alive = Some(keep_alive);
        self
    }

    /// Wrap as a client-side session.
    ///
    /// The protocol is already known from the negotiated subprotocol (ALPN), so
    /// this returns synchronously without awaiting in-band parameters.
    pub fn connect(self) -> Session {
        let (version, protocol) = alpn::parse(self.alpn.as_deref());
        let config = Config::negotiated(version, protocol);
        let transport = self.into_transport(config.version, config.max_record_size);
        Session::new(transport, false, config)
    }

    /// Wrap as a server-side session.
    ///
    /// As with [`connect`](Self::connect), the protocol is known from the
    /// negotiated subprotocol, so this returns synchronously.
    pub fn accept(self) -> Session {
        let (version, protocol) = alpn::parse(self.alpn.as_deref());
        let config = Config::negotiated(version, protocol);
        let transport = self.into_transport(config.version, config.max_record_size);
        Session::new(transport, true, config)
    }

    fn into_transport(self, version: Version, max_record_size: u64) -> WsTransport<T> {
        let transport = WsTransport::new(self.ws, version, max_record_size);
        match self.keep_alive {
            Some(ka) => transport.with_keep_alive(ka),
            None => transport,
        }
    }
}

/// A QMux client that opens a WebSocket and negotiates an application protocol.
///
/// Each entry pairs an `alpn` with the QMux wire-format `versions` it can
/// ride on. The wire form `{v.prefix()}{alpn}` is emitted once per listed
/// version, in order; an empty `versions` slice expands to every QMux draft
/// this crate knows about. Add multiple entries to negotiate across drafts in
/// a single handshake; the wire-format version comes from the negotiated
/// subprotocol. By default the bare version ALPNs (`qmux-01`, `qmux-00`,
/// `webtransport`) are also offered for peers that don't pin an app protocol;
/// call [`Client::require_protocol`] to offer only the configured pairs.
#[derive(Default, Clone)]
pub struct Client {
    protocols: Vec<(String, Vec<Version>)>,
    require_protocol: bool,
    config: Option<tungstenite::protocol::WebSocketConfig>,
    keep_alive: Option<KeepAlive>,
    #[cfg(feature = "wss")]
    connector: Option<tokio_tungstenite::Connector>,
}

impl Client {
    pub fn new() -> Self {
        Self::default()
    }

    /// Advertise `alpn` under the listed QMux wire-format versions.
    ///
    /// Pass `&[v]` to pin to one version, `&[v1, v2]` to enumerate, or `&[]`
    /// to let the polyfill expand to every QMux draft it knows about.
    pub fn with_protocol(mut self, alpn: &str, versions: &[Version]) -> Self {
        self.protocols.push((alpn.to_string(), versions.to_vec()));
        self
    }

    /// Advertise multiple `(alpn, versions)` entries in preference order.
    pub fn with_protocols<'a>(
        mut self,
        entries: impl IntoIterator<Item = (&'a str, &'a [Version])>,
    ) -> Self {
        self.protocols.extend(
            entries
                .into_iter()
                .map(|(a, vs)| (a.to_string(), vs.to_vec())),
        );
        self
    }

    /// Offer only the prefixed `(alpn, version)` pairs, suppressing the bare
    /// version ALPNs (`qmux-01`, `qmux-00`, `webtransport`) that are offered by
    /// default. Use this when the peer must commit to one of the application
    /// protocols you've configured.
    pub fn require_protocol(mut self) -> Self {
        self.require_protocol = true;
        self
    }

    /// Set the WebSocket configuration (e.g. max message/frame sizes).
    pub fn with_config(mut self, config: tungstenite::protocol::WebSocketConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// Send periodic Pings and close the session if the peer goes silent.
    ///
    /// WebSocket has no built-in idle timeout, so without this a crashed peer
    /// stays "connected" until OS-level TCP keep_alive eventually probes.
    pub fn with_keep_alive(mut self, keep_alive: KeepAlive) -> Self {
        self.keep_alive = Some(keep_alive);
        self
    }

    /// Set the TLS connector for secure WebSocket connections.
    #[cfg(feature = "wss")]
    pub fn with_connector(mut self, connector: tokio_tungstenite::Connector) -> Self {
        self.connector = Some(connector);
        self
    }

    /// Connect to a WebSocket server, negotiating an advertised `(alpn, version)`.
    pub async fn connect(&self, url: &str) -> Result<Session, Error> {
        use tungstenite::{client::IntoClientRequest, http};

        for (a, _) in &self.protocols {
            validate_protocol(a)?;
        }

        let mut request = url.into_client_request().map_err(Error::from)?;

        let entries = self
            .protocols
            .iter()
            .map(|(a, vs)| (a.as_str(), vs.as_slice()));
        let protocol_value = alpn::build(entries, self.require_protocol).join(", ");

        request.headers_mut().insert(
            http::header::SEC_WEBSOCKET_PROTOCOL,
            http::HeaderValue::from_str(&protocol_value)
                .map_err(|_| Error::InvalidProtocol(protocol_value))?,
        );

        #[cfg(feature = "wss")]
        let (ws_stream, response) = {
            tokio_tungstenite::connect_async_tls_with_config(
                request,
                self.config,
                false,
                self.connector.clone(),
            )
            .await
            .map_err(Error::from)?
        };

        #[cfg(not(feature = "wss"))]
        let (ws_stream, response) =
            tokio_tungstenite::connect_async_with_config(request, self.config, false)
                .await
                .map_err(Error::from)?;

        let negotiated = response
            .headers()
            .get(http::header::SEC_WEBSOCKET_PROTOCOL)
            .and_then(|h| h.to_str().ok());

        let (version, protocol) = alpn::parse(negotiated);

        // In strict mode an unrecognized or absent subprotocol would otherwise
        // fall through to the legacy `webtransport` wire format with no app
        // protocol, silently downgrading the negotiation we promised to enforce.
        if self.require_protocol && protocol.is_none() {
            return Err(Error::InvalidProtocol(
                negotiated.unwrap_or("<none>").to_string(),
            ));
        }

        let config = Config::negotiated(version, protocol);
        let transport = WsTransport::new(ws_stream, config.version, config.max_record_size);
        let transport = match self.keep_alive {
            Some(ka) => transport.with_keep_alive(ka),
            None => transport,
        };
        // Protocol came from the negotiated subprotocol, so no in-band wait.
        Ok(Session::new(transport, false, config))
    }
}

/// A QMux server that accepts WebSocket connections.
///
/// Each entry pairs an `alpn` with the QMux wire-format `versions` it can
/// ride on. The handshake callback matches each `{v.prefix()}{alpn}`
/// permutation against the client's offered `Sec-WebSocket-Protocol` in
/// declaration order. An empty `versions` slice expands to every QMux draft
/// this crate knows about. By default bare version ALPNs (`qmux-01`,
/// `qmux-00`, `webtransport`) are also accepted for peers that don't pin an
/// app protocol; call [`Server::require_protocol`] to accept only the
/// configured pairs.
#[derive(Default, Clone)]
pub struct Server {
    protocols: Vec<(String, Vec<Version>)>,
    require_protocol: bool,
    keep_alive: Option<KeepAlive>,
}

impl Server {
    pub fn new() -> Self {
        Self::default()
    }

    /// Advertise `alpn` under the listed QMux wire-format versions.
    ///
    /// Pass `&[v]` to support one version, `&[v1, v2]` to enumerate, or `&[]`
    /// to accept every QMux draft this crate knows about.
    pub fn with_protocol(mut self, alpn: &str, versions: &[Version]) -> Self {
        self.protocols.push((alpn.to_string(), versions.to_vec()));
        self
    }

    /// Advertise multiple `(alpn, versions)` entries in preference order.
    pub fn with_protocols<'a>(
        mut self,
        entries: impl IntoIterator<Item = (&'a str, &'a [Version])>,
    ) -> Self {
        self.protocols.extend(
            entries
                .into_iter()
                .map(|(a, vs)| (a.to_string(), vs.to_vec())),
        );
        self
    }

    /// Accept only the configured prefixed pairs, rejecting clients that offer
    /// just a bare version ALPN (`qmux-01`, `qmux-00`, `webtransport`) with no
    /// application protocol. By default those bare ALPNs are accepted, yielding
    /// a session with `Config::protocol == None` and the wire-format version
    /// implied by the ALPN.
    pub fn require_protocol(mut self) -> Self {
        self.require_protocol = true;
        self
    }

    /// Send periodic Pings and close the session if the peer goes silent.
    ///
    /// WebSocket has no built-in idle timeout, so without this a crashed peer
    /// stays "connected" until OS-level TCP keep_alive eventually probes.
    pub fn with_keep_alive(mut self, keep_alive: KeepAlive) -> Self {
        self.keep_alive = Some(keep_alive);
        self
    }

    /// Accept a WebSocket connection, negotiating an offered `(alpn, version)`.
    pub async fn accept<T: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
        &self,
        socket: T,
    ) -> Result<Session, Error> {
        use std::sync::{Arc, Mutex};
        use tungstenite::{handshake::server, http};

        for (a, _) in &self.protocols {
            validate_protocol(a)?;
        }

        let negotiated = Arc::new(Mutex::new(None::<(Version, Option<String>)>));
        let negotiated_clone = negotiated.clone();
        let supported = self.protocols.clone();
        let require_protocol = self.require_protocol;

        #[allow(clippy::result_large_err)]
        let callback = move |req: &server::Request,
                             mut response: server::Response|
              -> Result<server::Response, server::ErrorResponse> {
            let header_protocols: Vec<&str> = req
                .headers()
                .get_all(http::header::SEC_WEBSOCKET_PROTOCOL)
                .iter()
                .filter_map(|v| v.to_str().ok())
                .flat_map(|h| h.split(','))
                .map(|p| p.trim())
                .filter(|p| !p.is_empty())
                .collect();

            // Iterate supported entries in preference order; for each, expand
            // the listed versions (empty = every supported QMux draft) and pick
            // the first `{prefix}{alpn}` permutation the client offered.
            for (alpn, versions) in &supported {
                for &version in alpn::expand_versions(versions) {
                    let wire = format!("{}{}", version.prefix(), alpn);
                    if header_protocols.iter().any(|p| *p == wire) {
                        response.headers_mut().insert(
                            http::header::SEC_WEBSOCKET_PROTOCOL,
                            http::HeaderValue::from_str(&wire).unwrap(),
                        );
                        *negotiated_clone.lock().unwrap() = Some((version, Some(alpn.clone())));
                        return Ok(response);
                    }
                }
            }

            // Default fallback: accept bare version ALPNs (qmux-01, qmux-00,
            // webtransport) for clients that didn't request an app protocol,
            // unless require_protocol was set. The session ends up with
            // `protocol = None` and the wire-format version implied by
            // whichever bare ALPN won.
            if !require_protocol {
                for &version in alpn::BARE_ALPNS {
                    let bare = version.alpn();
                    if header_protocols.contains(&bare) {
                        response.headers_mut().insert(
                            http::header::SEC_WEBSOCKET_PROTOCOL,
                            http::HeaderValue::from_str(bare).unwrap(),
                        );
                        *negotiated_clone.lock().unwrap() = Some((version, None));
                        return Ok(response);
                    }
                }
            }

            Err(http::Response::builder()
                .status(http::StatusCode::BAD_REQUEST)
                .body(Some("no supported protocol".to_string()))
                .unwrap())
        };

        let ws = tokio_tungstenite::accept_hdr_async_with_config(socket, callback, None).await?;

        let (version, protocol) = negotiated
            .lock()
            .unwrap()
            .take()
            .expect("negotiated must be set after successful handshake");

        let config = Config::negotiated(version, protocol);
        let transport = WsTransport::new(ws, config.version, config.max_record_size);
        let transport = match self.keep_alive {
            Some(ka) => transport.with_keep_alive(ka),
            None => transport,
        };
        // Protocol came from the negotiated subprotocol, so no in-band wait.
        Ok(Session::new(transport, true, config))
    }
}
