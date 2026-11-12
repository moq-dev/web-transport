use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};

use crate::protocol::validate_protocol;
use crate::transport::WsTransport;
use crate::{alpn, Config, Error, Session, Version};

/// The `tokio-tungstenite` version this crate is built against.
///
/// [`Client::with_connector`] and [`Upgraded`] take `tokio-tungstenite` types
/// directly, so use this re-export rather than a separately pinned dependency —
/// a different minor version is a distinct crate to the compiler and fails to
/// typecheck against these APIs.
pub use tokio_tungstenite;

/// The `tungstenite` version this crate is built against, matching
/// [`tokio_tungstenite`].
///
/// [`Client::with_config`], [`Server::with_config`], and [`Upgraded`] name
/// `tungstenite` types, and [`Error::WebSocket`] wraps a `tungstenite::Error`.
pub use tokio_tungstenite::tungstenite;

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
    socket_stats: Option<crate::SharedSocketStats>,
    session: Config,
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
            socket_stats: None,
            session: Config::default(),
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

    /// Use `config` for every session setting except the wire-format version
    /// and application protocol, which the handshake negotiates.
    ///
    /// The session closes if the peer's transport parameters do not arrive within
    /// [`handshake_timeout`](Config::handshake_timeout). Keep
    /// [`max_record_size`](Config::max_record_size) within the WebSocket's
    /// frame and message limits, since each record is one message. QMux cannot
    /// inspect these limits on an externally upgraded WebSocket.
    pub fn with_session_config(mut self, config: Config) -> Self {
        self.session = config;
        self
    }

    /// Report statistics for the socket underneath the upgrade.
    ///
    /// An HTTP server hands over a type-erased upgraded stream, so QMux can't find
    /// the socket itself. A server that kept a handle on the accepted connection
    /// can attach it here and get the kernel's RTT (and, on Linux, delivery rate)
    /// instead of only QMux's own `QX_PING` measurement. See
    /// [`TcpStats`](crate::TcpStats).
    pub fn with_socket_stats(mut self, stats: crate::SharedSocketStats) -> Self {
        self.socket_stats = Some(stats);
        self
    }

    /// Wrap as a client-side session.
    ///
    /// The protocol is already known from the negotiated subprotocol (ALPN), so
    /// this returns synchronously without awaiting in-band parameters.
    pub fn connect(self) -> Session {
        let (version, protocol) = alpn::parse(self.alpn.as_deref());
        let config = self.session.clone().with_negotiated(version, protocol);
        let transport = self.into_transport(config.version, config.max_record_size);
        Session::new(transport, false, config)
    }

    /// Wrap as a server-side session.
    ///
    /// As with [`connect`](Self::connect), the protocol is known from the
    /// negotiated subprotocol, so this returns synchronously.
    pub fn accept(self) -> Session {
        let (version, protocol) = alpn::parse(self.alpn.as_deref());
        let config = self.session.clone().with_negotiated(version, protocol);
        let transport = self.into_transport(config.version, config.max_record_size);
        Session::new(transport, true, config)
    }

    fn into_transport(self, version: Version, max_record_size: u64) -> WsTransport<T> {
        let mut transport = WsTransport::new(self.ws, version, max_record_size);
        if let Some(stats) = self.socket_stats {
            transport = transport.with_socket_stats(stats);
        }
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
    session: Config,
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
    ///
    /// On record-framed drafts (QMux01+), the advertised record size is clamped
    /// to these limits; a limit below `DEFAULT_MAX_RECORD_SIZE` fails
    /// establishment locally, before any QMux parameters are sent.
    /// For bare-frame versions, callers must allow the peer's frame sizes:
    /// QMux00 permits up to `MAX_FRAME_SIZE`; legacy `webtransport`
    /// negotiates no size bound.
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

    /// Use `config` for every session setting except the wire-format version
    /// and application protocol, which the handshake negotiates.
    ///
    /// The session closes if the peer's transport parameters do not arrive within
    /// [`handshake_timeout`](Config::handshake_timeout). The advertised
    /// [`max_record_size`](Config::max_record_size) on record-framed drafts
    /// (QMux01+) is clamped to the WebSocket's frame and message limits (16 MiB
    /// and 64 MiB by default), since each record is one message. Establishment
    /// fails with [`Error::TransportParameter`] before any QMux parameters are
    /// sent if the resulting limit is below `DEFAULT_MAX_RECORD_SIZE`.
    pub fn with_session_config(mut self, config: Config) -> Self {
        self.session = config;
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
        let (mut ws_stream, response) = {
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
        let (mut ws_stream, response) =
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

        let config = session_config(
            self.session.clone().with_negotiated(version, protocol),
            self.config,
            &mut ws_stream,
        )
        .await?;
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
    config: Option<tungstenite::protocol::WebSocketConfig>,
    keep_alive: Option<KeepAlive>,
    session: Config,
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

    /// Set the WebSocket configuration (e.g. max message/frame sizes).
    ///
    /// On record-framed drafts (QMux01+), the advertised record size is clamped
    /// to these limits; a limit below `DEFAULT_MAX_RECORD_SIZE` fails
    /// establishment locally, before any QMux parameters are sent.
    /// For bare-frame versions, callers must allow the peer's frame sizes:
    /// QMux00 permits up to `MAX_FRAME_SIZE`; legacy `webtransport`
    /// negotiates no size bound.
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

    /// Use `config` for every session setting except the wire-format version
    /// and application protocol, which the handshake negotiates.
    ///
    /// The session closes if the peer's transport parameters do not arrive within
    /// [`handshake_timeout`](Config::handshake_timeout). The advertised
    /// [`max_record_size`](Config::max_record_size) on record-framed drafts
    /// (QMux01+) is clamped to the WebSocket's frame and message limits (16 MiB
    /// and 64 MiB by default), since each record is one message. Establishment
    /// fails with [`Error::TransportParameter`] before any QMux parameters are
    /// sent if the resulting limit is below `DEFAULT_MAX_RECORD_SIZE`.
    pub fn with_session_config(mut self, config: Config) -> Self {
        self.session = config;
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

        let mut ws =
            tokio_tungstenite::accept_hdr_async_with_config(socket, callback, self.config).await?;

        let (version, protocol) = negotiated
            .lock()
            .unwrap()
            .take()
            .expect("negotiated must be set after successful handshake");

        let config = session_config(
            self.session.clone().with_negotiated(version, protocol),
            self.config,
            &mut ws,
        )
        .await?;
        let transport = WsTransport::new(ws, config.version, config.max_record_size);
        let transport = match self.keep_alive {
            Some(ka) => transport.with_keep_alive(ka),
            None => transport,
        };
        // Protocol came from the negotiated subprotocol, so no in-band wait.
        Ok(Session::new(transport, true, config))
    }
}

async fn session_config<T: AsyncRead + AsyncWrite + Unpin>(
    mut session: Config,
    websocket: Option<tungstenite::protocol::WebSocketConfig>,
    stream: &mut tokio_tungstenite::WebSocketStream<T>,
) -> Result<Config, Error> {
    if !session.version.uses_records() {
        return Ok(session);
    }
    let websocket = websocket.unwrap_or_default();
    for limit in [websocket.max_frame_size, websocket.max_message_size]
        .into_iter()
        .flatten()
    {
        session.max_record_size = session.max_record_size.min(limit as u64);
    }
    if session.max_record_size < crate::DEFAULT_MAX_RECORD_SIZE {
        stream
            .close(Some(tungstenite::protocol::CloseFrame {
                code: tungstenite::protocol::frame::coding::CloseCode::Error,
                reason: "QMux record limit below minimum".into(),
            }))
            .await?;
        return Err(Error::TransportParameter);
    }
    Ok(session)
}

#[cfg(test)]
mod record_size_tests {
    use super::*;
    use crate::proto::{Frame, TransportParams, DEFAULT_MAX_RECORD_SIZE};
    use bytes::Bytes;
    use futures::{SinkExt, StreamExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio_tungstenite::WebSocketStream;
    use tungstenite::{client::IntoClientRequest, protocol::WebSocketConfig, Message};
    use web_transport_trait::Session as _;

    /// Exercise either builder against a raw peer that can inspect its parameters.
    async fn session_peer(
        server: bool,
        websocket: Option<WebSocketConfig>,
        config: Config,
    ) -> (Result<Session, Error>, WebSocketStream<TcpStream>) {
        let version = config.version;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/", listener.local_addr().unwrap());
        if server {
            let mut builder = Server::new().with_session_config(config);
            if let Some(websocket) = websocket {
                builder = builder.with_config(websocket);
            }
            let accept = async {
                let (socket, _) = listener.accept().await.unwrap();
                builder.accept(socket).await
            };
            let connect = async {
                let socket = TcpStream::connect(listener.local_addr().unwrap())
                    .await
                    .unwrap();
                let mut request = url.into_client_request().unwrap();
                request
                    .headers_mut()
                    .insert("Sec-WebSocket-Protocol", version.alpn().parse().unwrap());
                tokio_tungstenite::client_async(request, socket)
                    .await
                    .unwrap()
                    .0
            };
            tokio::join!(accept, connect)
        } else {
            let mut builder = Client::new().with_session_config(config);
            if version.is_qmux() {
                builder = builder.with_protocol("test", &[version]).require_protocol();
            }
            if let Some(websocket) = websocket {
                builder = builder.with_config(websocket);
            }
            let accept = async {
                let (socket, _) = listener.accept().await.unwrap();
                #[allow(clippy::result_large_err)]
                let callback =
                    |_: &tungstenite::handshake::server::Request,
                     mut response: tungstenite::handshake::server::Response| {
                        let protocol = if version.is_qmux() {
                            format!("{}test", version.prefix())
                        } else {
                            version.alpn().to_string()
                        };
                        response
                            .headers_mut()
                            .insert("Sec-WebSocket-Protocol", protocol.parse().unwrap());
                        Ok(response)
                    };
                tokio_tungstenite::accept_hdr_async(socket, callback)
                    .await
                    .unwrap()
            };
            let (session, peer) = tokio::join!(builder.connect(&url), accept);
            (session, peer)
        }
    }

    async fn advertised(
        peer: &mut WebSocketStream<TcpStream>,
        version: Version,
    ) -> TransportParams {
        let message = peer.next().await.unwrap().unwrap().into_data();
        let Some(Frame::TransportParameters(params)) = Frame::decode(message, version).unwrap()
        else {
            panic!("expected transport parameters");
        };
        params
    }

    #[tokio::test]
    async fn builders_advertise_the_effective_record_and_datagram_limits() {
        let min = DEFAULT_MAX_RECORD_SIZE;
        let unlimited = WebSocketConfig::default()
            .max_frame_size(None)
            .max_message_size(None);
        let limits =
            |frame, message| Some(unlimited.max_frame_size(frame).max_message_size(message));
        let bytes = min as usize;
        let cases = [
            // Defaults cap a raised session limit at the 16 MiB frame limit.
            (None, 32 << 20, 16 << 20),
            (None, min, min),
            (Some(unlimited), 32 << 20, 32 << 20),
            (limits(Some(bytes), None), 2 * min, min),
            (limits(None, Some(bytes)), 2 * min, min),
            (limits(Some(2 * bytes), Some(bytes)), 3 * min, min),
            (limits(Some(bytes), Some(2 * bytes)), 3 * min, min),
            (limits(Some(3 * bytes), Some(2 * bytes)), min, min),
        ];
        for server in [false, true] {
            for version in [Version::QMux01, Version::QMux02] {
                for (websocket, requested, expected) in cases {
                    let mut config = Config::new(version);
                    config.max_record_size = requested;
                    config.max_datagram_frame_size = requested;
                    let (session, mut peer) = session_peer(server, websocket, config).await;
                    let session = session.unwrap();
                    let params = advertised(&mut peer, version).await;
                    assert_eq!(
                        params.max_record_size, expected,
                        "server={server}, {websocket:?}"
                    );
                    assert_eq!(params.max_datagram_frame_size, expected);
                    session.close(0, "done");
                }
            }
        }
    }

    #[tokio::test]
    async fn records_at_the_websocket_limit_are_delivered() {
        let min = DEFAULT_MAX_RECORD_SIZE as usize;
        for server in [false, true] {
            for version in [Version::QMux01, Version::QMux02] {
                // Exercise both tungstenite bounds, including a fragmented message.
                for frame_limit in [true, false] {
                    let websocket = WebSocketConfig::default()
                        .max_frame_size(frame_limit.then_some(min))
                        .max_message_size((!frame_limit).then_some(min));
                    let mut config = Config::new(version);
                    config.max_record_size = 2 * min as u64;
                    config.max_datagram_frame_size = 2 * min as u64;
                    let (session, mut peer) = session_peer(server, Some(websocket), config).await;
                    let session = session.unwrap();
                    let params = advertised(&mut peer, version).await;
                    assert_eq!(params.max_record_size, min as u64);
                    peer.send(Message::Binary(
                        Frame::TransportParameters(Config::new(version).to_transport_params())
                            .encode(version)
                            .unwrap(),
                    ))
                    .await
                    .unwrap();
                    // A lengthless DATAGRAM fills exactly one advertised record.
                    let mut record = vec![0x5a; min];
                    record[0] = 0x30;
                    if frame_limit {
                        peer.send(Message::Binary(Bytes::from(record)))
                            .await
                            .unwrap();
                    } else {
                        use tungstenite::protocol::frame::{
                            coding::{Data, OpCode},
                            Frame as WsFrame,
                        };
                        peer.send(Message::Frame(WsFrame::message(
                            record[..min / 2].to_vec(),
                            OpCode::Data(Data::Binary),
                            false,
                        )))
                        .await
                        .unwrap();
                        peer.send(Message::Frame(WsFrame::message(
                            record[min / 2..].to_vec(),
                            OpCode::Data(Data::Continue),
                            true,
                        )))
                        .await
                        .unwrap();
                    }
                    assert_eq!(
                        session.recv_datagram().await.unwrap(),
                        Bytes::from(vec![0x5a; min - 1])
                    );
                    // One byte beyond the effective bound must still be rejected by
                    // the actual WebSocket config, including on the server.
                    let mut record = vec![0x5a; min + 1];
                    record[0] = 0x30;
                    peer.send(Message::Binary(Bytes::from(record)))
                        .await
                        .unwrap();
                    let error = session.closed().await;
                    assert!(
                        matches!(&error, Error::WebSocket(e) if matches!(e.as_ref(), tungstenite::Error::Capacity(_))),
                        "{error:?}"
                    );
                }
            }
        }
    }

    async fn assert_local_config_close(peer: &mut WebSocketStream<TcpStream>) {
        let Message::Close(Some(close)) = peer.next().await.unwrap().unwrap() else {
            panic!("expected a WebSocket Close without QMux parameters");
        };
        assert_eq!(
            close.code,
            tungstenite::protocol::frame::coding::CloseCode::Error
        );
        assert_eq!(close.reason.as_str(), "QMux record limit below minimum");
    }

    #[tokio::test]
    async fn invalid_record_limits_fail_without_advertising_parameters() {
        let min = DEFAULT_MAX_RECORD_SIZE as usize;
        for server in [false, true] {
            for version in [Version::QMux01, Version::QMux02] {
                for limit in [0, min - 1] {
                    for websocket in [
                        WebSocketConfig::default().max_frame_size(Some(limit)),
                        WebSocketConfig::default().max_message_size(Some(limit)),
                    ] {
                        let (session, mut peer) =
                            session_peer(server, Some(websocket), Config::new(version)).await;
                        assert!(matches!(session, Err(Error::TransportParameter)));
                        // The WebSocket negotiated a record-framed draft, but no
                        // invalid QMux transport parameters were put on the wire.
                        assert_local_config_close(&mut peer).await;
                    }
                }
                let mut config = Config::new(version);
                config.max_record_size = DEFAULT_MAX_RECORD_SIZE - 1;
                let (session, mut peer) = session_peer(server, None, config).await;
                assert!(matches!(session, Err(Error::TransportParameter)));
                assert_local_config_close(&mut peer).await;
            }
        }
    }

    #[tokio::test]
    async fn bare_frame_versions_do_not_require_record_limits() {
        use web_transport_trait::{RecvStream as _, SendStream as _};
        for server in [false, true] {
            for version in [Version::QMux00, Version::WebTransport] {
                for limit in [8192, DEFAULT_MAX_RECORD_SIZE as usize, 16383] {
                    let websocket = WebSocketConfig::default()
                        .max_frame_size(Some(limit))
                        .max_message_size(Some(limit));
                    let mut config = Config::new(version);
                    config.max_record_size = 1000; // Ignored by bare-frame versions.
                    let (session, peer) = session_peer(server, Some(websocket), config).await;
                    let session = session.unwrap();
                    let peer = Upgraded::new(peer).with_alpn(version.alpn());
                    let peer = if server {
                        peer.connect()
                    } else {
                        peer.accept()
                    };
                    let mut send = peer.open_uni().await.unwrap();
                    send.write(b"bare frame").await.unwrap();
                    send.finish().unwrap();
                    let mut recv = session.accept_uni().await.unwrap();
                    assert_eq!(
                        recv.read_all().await.unwrap(),
                        Bytes::from_static(b"bare frame")
                    );
                    session.close(0, "done");
                    peer.close(0, "done");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{Sink, SinkExt, Stream, StreamExt};
    use std::{future::Future, task::Context};
    use tokio::time::{advance, timeout};
    use tokio_tungstenite::{tungstenite::client::IntoClientRequest, WebSocketStream};
    use web_transport_trait::Session as _;

    const DEADLINE: Duration = Duration::from_secs(5);

    trait Peer:
        Stream<Item = Result<tungstenite::Message, tungstenite::Error>>
        + Sink<tungstenite::Message, Error = tungstenite::Error>
        + Unpin
    {
    }

    impl<T> Peer for WebSocketStream<T> where T: AsyncRead + AsyncWrite + Unpin {}

    #[derive(Clone, Copy, Debug)]
    enum Builder {
        Client,
        Server,
        UpgradedClient,
        UpgradedServer,
    }

    const BUILDERS: &[Builder] = &[
        Builder::Client,
        Builder::Server,
        Builder::UpgradedClient,
        Builder::UpgradedServer,
    ];

    fn config() -> Config {
        Config {
            handshake_timeout: DEADLINE,
            max_idle_timeout: 0,
            rtt_interval: Duration::ZERO,
            ..Config::default()
        }
    }

    async fn start(builder: Builder, version: Version, config: Config) -> (Session, Box<dyn Peer>) {
        use tungstenite::{http::header::SEC_WEBSOCKET_PROTOCOL, protocol::Role};

        if let Builder::Client = builder {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}", listener.local_addr().unwrap());
            let server = async {
                let (socket, _) = listener.accept().await.unwrap();
                #[allow(clippy::result_large_err)]
                let callback =
                    move |request: &tungstenite::handshake::server::Request,
                     mut response: tungstenite::handshake::server::Response| {
                        let protocol = if version.is_qmux() {
                            request.headers()[SEC_WEBSOCKET_PROTOCOL].clone()
                        } else {
                            version.alpn().parse().unwrap()
                        };
                        response.headers_mut().insert(SEC_WEBSOCKET_PROTOCOL, protocol);
                        Ok(response)
                    };
                tokio_tungstenite::accept_hdr_async(socket, callback)
                    .await
                    .unwrap()
            };
            let client = Client::new().with_session_config(config);
            let client = if version.is_qmux() {
                client.with_protocol("test", &[version]).require_protocol()
            } else {
                client
            };
            let (session, peer) = tokio::join!(client.connect(&url), server);
            return (session.unwrap(), Box::new(peer));
        }

        let (a, b) = tokio::io::duplex(64 * 1024);
        if let Builder::Server = builder {
            let server = Server::new().with_session_config(config);
            let mut request = "ws://localhost".into_client_request().unwrap();
            request
                .headers_mut()
                .insert(SEC_WEBSOCKET_PROTOCOL, version.alpn().parse().unwrap());
            let (session, peer) = tokio::join!(
                server.accept(a),
                tokio_tungstenite::client_async(request, b)
            );
            return (session.unwrap(), Box::new(peer.unwrap().0));
        }

        let is_server = matches!(builder, Builder::UpgradedServer);
        let (role, peer_role) = if is_server {
            (Role::Server, Role::Client)
        } else {
            (Role::Client, Role::Server)
        };
        let socket = WebSocketStream::from_raw_socket(a, role, None).await;
        let peer = WebSocketStream::from_raw_socket(b, peer_role, None).await;
        let upgraded = Upgraded::new(socket)
            .with_alpn(version.alpn())
            .with_session_config(config);
        let session = if is_server {
            upgraded.accept()
        } else {
            upgraded.connect()
        };
        (session, Box::new(peer))
    }

    fn assert_open(session: &Session) {
        let mut closed = std::pin::pin!(session.closed());
        assert!(closed
            .as_mut()
            .poll(&mut Context::from_waker(std::task::Waker::noop()))
            .is_pending());
    }

    async fn network_io<F: Future>(builder: Builder, io: F) -> F::Output {
        // Real socket delivery must not race paused time's automatic advance.
        let real_time = matches!(builder, Builder::Client);
        if real_time {
            tokio::time::resume();
        }
        let output = io.await;
        if real_time {
            tokio::time::pause();
        }
        output
    }

    #[tokio::test(start_paused = true)]
    async fn silent_peer_hits_handshake_deadline() {
        for &builder in BUILDERS {
            for &version in Version::ALL.iter().filter(|v| v.is_qmux()) {
                let (session, _peer) = start(builder, version, config()).await;
                advance(DEADLINE - Duration::from_millis(1)).await;
                assert_open(&session);
                let error = timeout(Duration::from_secs(1), session.closed())
                    .await
                    .unwrap_or_else(|_| panic!("{builder:?} {version:?}: handshake stayed open"));
                assert!(matches!(error, Error::HandshakeTimeout), "{error:?}");
                assert!(matches!(
                    session.open_uni().await,
                    Err(Error::HandshakeTimeout)
                ));
                assert!(matches!(
                    session.accept_uni().await,
                    Err(Error::HandshakeTimeout)
                ));
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn disabled_handshake_deadline_and_legacy_stay_open() {
        for &builder in BUILDERS {
            for &version in Version::ALL {
                let mut config = config();
                if version.is_qmux() {
                    config.handshake_timeout = Duration::ZERO;
                }
                let (session, _peer) = start(builder, version, config).await;
                advance(DEADLINE * 10).await;
                assert_open(&session);
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn oversized_handshake_deadline_stays_open() {
        for &builder in BUILDERS {
            for &version in Version::ALL.iter().filter(|v| v.is_qmux()) {
                let mut config = config();
                config.handshake_timeout = Duration::MAX;
                let (session, _peer) = start(builder, version, config).await;
                advance(DEADLINE * 10).await;
                assert_open(&session);
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn handshake_deadline_sends_connection_close() {
        for &version in Version::ALL.iter().filter(|v| v.is_qmux()) {
            let (session, mut peer) = start(Builder::UpgradedClient, version, config()).await;
            // Drain our parameters so the writer is idle when the deadline fires.
            assert!(matches!(
                peer.next().await.unwrap().unwrap(),
                tungstenite::Message::Binary(_)
            ));
            let error = timeout(DEADLINE * 2, session.closed()).await.unwrap();
            assert!(matches!(error, Error::HandshakeTimeout));
            let message = timeout(DEADLINE, peer.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let tungstenite::Message::Binary(data) = message else {
                panic!("expected a QMux CONNECTION_CLOSE, got {message:?}");
            };
            let Some(crate::Frame::ConnectionClose(close)) =
                crate::Frame::decode(data, version).unwrap()
            else {
                panic!("expected a QMux CONNECTION_CLOSE");
            };
            assert_eq!(close.code.into_inner(), 0);
            assert_eq!(close.reason, Error::HandshakeTimeout.to_string());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn receiving_parameters_cancels_handshake_deadline() {
        for &builder in BUILDERS {
            for &version in Version::ALL.iter().filter(|v| v.is_qmux()) {
                let (session, mut peer) = start(builder, version, config()).await;
                advance(DEADLINE / 2).await;
                let mut peer_config = config();
                peer_config.version = version;
                let params = crate::Frame::TransportParameters(peer_config.to_transport_params())
                    .encode(version)
                    .unwrap();
                let _stream = network_io(builder, async {
                    peer.send(tungstenite::Message::Binary(params))
                        .await
                        .unwrap();
                    // Opening acknowledges that the parameters were applied.
                    session.open_uni().await.unwrap()
                })
                .await;
                advance(DEADLINE * 2).await;
                assert_open(&session);
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn disconnect_before_parameters_preserves_error() {
        for &builder in BUILDERS {
            let (session, peer) = start(builder, Version::QMux02, config()).await;
            let error = network_io(builder, async {
                drop(peer);
                timeout(DEADLINE / 2, session.closed()).await.unwrap()
            })
            .await;
            assert!(!matches!(error, Error::HandshakeTimeout), "{error:?}");
        }
    }
}
