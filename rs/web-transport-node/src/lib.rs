use std::sync::Arc;

use napi::bindgen_prelude::*;
use napi_derive::napi;

use tokio::sync::Mutex;

// Teardown safety: every `#[napi] async fn` below runs on napi-rs's global tokio
// runtime. On `process.exit()` napi tears that runtime down with
// `Runtime::shutdown_background()` (non-blocking) and then finalizes the wrapped
// Rust objects -- so a worker thread can still be dropping a parked task (e.g. a
// `read()` blocked on the network) *after* the object it borrowed has been freed.
// If that task's future borrows `&self`, the drop dereferences freed memory and
// the process SIGSEGVs on exit (a use-after-free in `tokio-rt-worker`).
//
// To make those forced drops memory-safe, each async method clones the handle it
// needs (an `Arc<Mutex<..>>` for the non-`Clone` streams, or the cheap `Clone`
// handle for `Session`/`Client`) *before* its first `.await`. The `&self` borrow
// then ends before any suspension point, so the future owns everything it touches
// and a teardown-time drop only ever frees live memory. Do not "simplify" these
// clones back into `self.inner.lock()` across an await -- that reintroduces the
// crash.

fn session_error_to_close_info(err: &web_transport_quinn::SessionError) -> NapiCloseInfo {
    match err {
        web_transport_quinn::SessionError::WebTransportError(
            web_transport_quinn::WebTransportError::Closed(code, reason),
        ) => NapiCloseInfo {
            close_code: *code,
            reason: reason.clone(),
        },
        other => NapiCloseInfo {
            close_code: 0,
            reason: other.to_string(),
        },
    }
}

/// The origin of a stream failure, matching W3C `WebTransportErrorSource`.
#[napi(string_enum = "lowercase")]
pub enum NapiErrorSource {
    /// The peer reset or stopped this stream.
    Stream,
    /// The whole session failed.
    Session,
}

/// A stream failure carrying the fields of W3C `WebTransportError`.
#[napi(object)]
pub struct NapiStreamError {
    pub source: NapiErrorSource,
    /// The peer's RESET_STREAM or STOP_SENDING code, or null for a session failure or a code
    /// outside the WebTransport range.
    pub stream_error_code: Option<u32>,
    pub message: String,
}

impl NapiStreamError {
    fn stream(code: Option<u32>, message: String) -> Self {
        Self {
            source: NapiErrorSource::Stream,
            stream_error_code: code,
            message,
        }
    }

    fn session(err: &web_transport_quinn::SessionError) -> Self {
        Self {
            source: NapiErrorSource::Session,
            stream_error_code: None,
            message: err.to_string(),
        }
    }
}

/// A WebTransport client that can connect to servers.
#[napi]
pub struct NapiClient {
    inner: web_transport_quinn::Client,
}

#[napi]
impl NapiClient {
    /// Create a client that validates server certificates against system root CAs.
    #[napi(factory)]
    pub fn with_system_roots() -> Result<Self> {
        napi::bindgen_prelude::within_runtime_if_available(|| {
            let client = web_transport_quinn::ClientBuilder::new()
                .with_system_roots()
                .map_err(|e| Error::from_reason(e.to_string()))?;
            Ok(Self { inner: client })
        })
    }

    /// Create a client that skips certificate verification entirely.
    /// WARNING: Only use for testing with self-signed certificates.
    #[napi(factory)]
    pub fn disable_verify() -> Result<Self> {
        napi::bindgen_prelude::within_runtime_if_available(|| {
            let client = web_transport_quinn::ClientBuilder::new()
                .dangerous()
                .with_no_certificate_verification()
                .map_err(|e| Error::from_reason(e.to_string()))?;
            Ok(Self { inner: client })
        })
    }

    /// Create a client that validates server certificates by SHA-256 hash.
    #[napi(factory)]
    pub fn with_certificate_hashes(hashes: Vec<Buffer>) -> Result<Self> {
        napi::bindgen_prelude::within_runtime_if_available(|| {
            let hashes: Vec<Vec<u8>> = hashes.into_iter().map(|b| b.to_vec()).collect();
            let client = web_transport_quinn::ClientBuilder::new()
                .with_server_certificate_hashes(hashes)
                .map_err(|e| Error::from_reason(e.to_string()))?;
            Ok(Self { inner: client })
        })
    }

    /// Connect to a WebTransport server at the given URL.
    #[napi]
    pub async fn connect(
        &self,
        url_str: String,
        options: Option<NapiConnectOptions>,
    ) -> Result<NapiSession> {
        // Own the client handle for the duration of the future (see module note).
        let client = self.inner.clone();
        let url: url::Url = url_str
            .parse()
            .map_err(|e: url::ParseError| Error::from_reason(e.to_string()))?;
        let mut request = web_transport_quinn::proto::ConnectRequest::new(url);
        if let Some(opts) = options {
            if let Some(protocols) = opts.protocols {
                request = request.with_protocols(protocols);
            }
        }
        let session = client
            .connect(request)
            .await
            .map_err(|e| Error::from_reason(e.to_string()))?;
        Ok(NapiSession {
            inner: session.clone(),
            closed: Arc::new(Mutex::new(None)),
        })
    }
}

/// Options for connecting to a WebTransport server.
#[napi(object)]
pub struct NapiConnectOptions {
    /// Subprotocols for WT-Available-Protocols negotiation.
    pub protocols: Option<Vec<String>>,
}

/// A WebTransport server that accepts incoming sessions.
#[napi]
pub struct NapiServer {
    inner: Arc<Mutex<Option<web_transport_quinn::Server>>>,
}

#[napi]
impl NapiServer {
    /// Create a server bound to the given address with the given TLS certificate.
    #[napi(factory)]
    pub fn bind(addr: String, cert_pem: Buffer, key_pem: Buffer) -> Result<Self> {
        napi::bindgen_prelude::within_runtime_if_available(|| {
            let certs = rustls_pemfile::certs(&mut &cert_pem[..])
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| Error::from_reason(format!("invalid certificate PEM: {e}")))?;
            let key = rustls_pemfile::private_key(&mut &key_pem[..])
                .map_err(|e| Error::from_reason(format!("invalid private key PEM: {e}")))?
                .ok_or_else(|| Error::from_reason("no private key found in PEM"))?;

            let addr: std::net::SocketAddr = addr
                .parse()
                .map_err(|e: std::net::AddrParseError| Error::from_reason(e.to_string()))?;

            let server = web_transport_quinn::ServerBuilder::new()
                .with_addr(addr)
                .with_certificate(certs, key)
                .map_err(|e| Error::from_reason(e.to_string()))?;

            Ok(Self {
                inner: Arc::new(Mutex::new(Some(server))),
            })
        })
    }

    /// Accept the next incoming WebTransport session request.
    #[napi]
    pub async fn accept(&self) -> Result<Option<NapiRequest>> {
        // Own the shared state for the duration of the future (see module note).
        let inner = self.inner.clone();
        let mut guard = inner.lock().await;
        let server = match guard.as_mut() {
            Some(server) => server,
            None => return Ok(None),
        };
        match server.accept().await {
            Some(request) => Ok(Some(NapiRequest {
                inner: Arc::new(Mutex::new(Some(request))),
            })),
            None => {
                guard.take();
                Ok(None)
            }
        }
    }

    /// Close the server, stopping it from accepting new connections.
    #[napi]
    pub fn close(&self) {
        within_runtime_if_available(|| {
            self.inner.blocking_lock().take();
        });
    }
}

/// A pending WebTransport session request from a client.
#[napi]
pub struct NapiRequest {
    // Option so we can take it in ok()/reject() which consume the Request.
    inner: Arc<Mutex<Option<web_transport_quinn::Request>>>,
}

#[napi]
impl NapiRequest {
    /// Get the URL of the CONNECT request.
    #[napi(getter)]
    pub async fn url(&self) -> Result<String> {
        let inner = self.inner.clone();
        let guard = inner.lock().await;
        let request = guard
            .as_ref()
            .ok_or_else(|| Error::from_reason("request already consumed"))?;
        Ok(request.url.to_string())
    }

    /// Accept the session with 200 OK.
    #[napi]
    pub async fn ok(&self) -> Result<NapiSession> {
        let inner = self.inner.clone();
        let request = inner
            .lock()
            .await
            .take()
            .ok_or_else(|| Error::from_reason("request already consumed"))?;
        let session = request
            .ok()
            .await
            .map_err(|e| Error::from_reason(e.to_string()))?;
        Ok(NapiSession {
            inner: session.clone(),
            closed: Arc::new(Mutex::new(None)),
        })
    }

    /// Reject the session with the given HTTP status code.
    #[napi]
    pub async fn reject(&self, status: u16) -> Result<()> {
        let inner = self.inner.clone();
        let request = inner
            .lock()
            .await
            .take()
            .ok_or_else(|| Error::from_reason("request already consumed"))?;
        let status = http::StatusCode::from_u16(status)
            .map_err(|e| Error::from_reason(format!("invalid status code: {e}")))?;
        request
            .reject(status)
            .await
            .map_err(|e| Error::from_reason(e.to_string()))?;
        Ok(())
    }
}

/// An established WebTransport session.
#[napi]
pub struct NapiSession {
    inner: web_transport_quinn::Session,
    // Cache the closed future result so multiple callers can await it.
    closed: Arc<Mutex<Option<NapiCloseInfo>>>,
}

/// Info about why a session was closed, matching W3C WebTransportCloseInfo.
#[derive(Clone)]
#[napi(object)]
pub struct NapiCloseInfo {
    pub close_code: u32,
    pub reason: String,
}

/// A bidirectional stream pair.
#[napi]
pub struct NapiBiStream {
    send: Option<NapiSendStream>,
    recv: Option<NapiRecvStream>,
}

#[napi]
impl NapiBiStream {
    /// Take the send stream. Can only be called once.
    #[napi]
    pub fn take_send(&mut self) -> Result<NapiSendStream> {
        self.send
            .take()
            .ok_or_else(|| Error::from_reason("send stream already taken"))
    }

    /// Take the recv stream. Can only be called once.
    #[napi]
    pub fn take_recv(&mut self) -> Result<NapiRecvStream> {
        self.recv
            .take()
            .ok_or_else(|| Error::from_reason("recv stream already taken"))
    }
}

#[napi]
impl NapiSession {
    /// The subprotocol selected by the server during WT-Available-Protocols negotiation.
    #[napi(getter)]
    pub fn protocol(&self) -> Option<String> {
        self.inner.protocol().map(str::to_string)
    }

    /// Accept an incoming unidirectional stream.
    #[napi]
    pub async fn accept_uni(&self) -> Result<NapiRecvStream> {
        // Own the session handle for the duration of the future (see module note).
        let session = self.inner.clone();
        let recv = session
            .accept_uni()
            .await
            .map_err(|e| Error::from_reason(e.to_string()))?;
        Ok(NapiRecvStream {
            inner: Arc::new(Mutex::new(recv)),
        })
    }

    /// Accept an incoming bidirectional stream.
    #[napi]
    pub async fn accept_bi(&self) -> Result<NapiBiStream> {
        let session = self.inner.clone();
        let (send, recv) = session
            .accept_bi()
            .await
            .map_err(|e| Error::from_reason(e.to_string()))?;
        Ok(NapiBiStream {
            send: Some(NapiSendStream {
                inner: Arc::new(Mutex::new(send)),
            }),
            recv: Some(NapiRecvStream {
                inner: Arc::new(Mutex::new(recv)),
            }),
        })
    }

    /// Open a new unidirectional stream.
    #[napi]
    pub async fn open_uni(&self) -> Result<NapiSendStream> {
        let session = self.inner.clone();
        let send = session
            .open_uni()
            .await
            .map_err(|e| Error::from_reason(e.to_string()))?;
        Ok(NapiSendStream {
            inner: Arc::new(Mutex::new(send)),
        })
    }

    /// Open a new bidirectional stream.
    #[napi]
    pub async fn open_bi(&self) -> Result<NapiBiStream> {
        let session = self.inner.clone();
        let (send, recv) = session
            .open_bi()
            .await
            .map_err(|e| Error::from_reason(e.to_string()))?;
        Ok(NapiBiStream {
            send: Some(NapiSendStream {
                inner: Arc::new(Mutex::new(send)),
            }),
            recv: Some(NapiRecvStream {
                inner: Arc::new(Mutex::new(recv)),
            }),
        })
    }

    /// Send a datagram.
    #[napi]
    pub fn send_datagram(&self, data: Buffer) -> Result<()> {
        within_runtime_if_available(|| {
            self.inner
                .send_datagram(bytes::Bytes::from(data.to_vec()))
                .map_err(|e| Error::from_reason(e.to_string()))
        })
    }

    /// Receive a datagram.
    #[napi]
    pub async fn recv_datagram(&self) -> Result<Buffer> {
        let session = self.inner.clone();
        let data = session
            .read_datagram()
            .await
            .map_err(|e| Error::from_reason(e.to_string()))?;
        Ok(Buffer::from(data.to_vec()))
    }

    /// Get the maximum datagram size.
    #[napi]
    pub fn max_datagram_size(&self) -> u32 {
        within_runtime_if_available(|| self.inner.max_datagram_size() as u32)
    }

    /// Close the session with a code and reason.
    #[napi]
    pub fn close(&self, code: u32, reason: String) {
        within_runtime_if_available(|| {
            self.inner.close(code, reason.as_bytes());
        });
    }

    /// Wait for the session to close, returning close info matching W3C WebTransportCloseInfo.
    #[napi]
    pub async fn closed(&self) -> Result<NapiCloseInfo> {
        // Own the session handle and the cache for the duration of the future
        // (see module note).
        let session = self.inner.clone();
        let cache = self.closed.clone();

        // Check if we already have a cached result.
        {
            let cached = cache.lock().await;
            if let Some(info) = cached.as_ref() {
                return Ok(info.clone());
            }
        }

        let err = session.closed().await;
        let info = session_error_to_close_info(&err);

        // Cache the result.
        {
            let mut cached = cache.lock().await;
            *cached = Some(info.clone());
        }

        Ok(info)
    }
}

/// A send stream for writing data.
#[napi]
pub struct NapiSendStream {
    inner: Arc<Mutex<web_transport_quinn::SendStream>>,
}

#[napi]
impl NapiSendStream {
    /// Write data to the stream, returning the error if the peer stopped it or the session failed.
    #[napi]
    pub async fn write(&self, data: Buffer) -> Result<Option<NapiStreamError>> {
        use web_transport_quinn::WriteError;

        // Own the shared stream for the duration of the future (see module note).
        let inner = self.inner.clone();
        let mut stream = inner.lock().await;
        let err = match stream.write_all(&data).await {
            Ok(()) => return Ok(None),
            Err(err @ WriteError::Stopped(code)) => {
                NapiStreamError::stream(Some(code), err.to_string())
            }
            Err(err @ WriteError::InvalidStopped(_)) => {
                NapiStreamError::stream(None, err.to_string())
            }
            Err(WriteError::SessionError(err)) => NapiStreamError::session(&err),
            Err(err @ WriteError::ClosedStream) => return Err(Error::from_reason(err.to_string())),
        };
        Ok(Some(err))
    }

    /// Watch for the stream to close. Call before any write, since it takes the stream lock.
    ///
    /// The watch is its own object because a pending call keeps its receiver alive: awaiting
    /// on this stream would keep an abandoned one from dropping and sending its implicit FIN.
    #[napi]
    pub fn closed(&self) -> Result<NapiSendClosed> {
        let stream = self
            .inner
            .try_lock()
            .map_err(|_| Error::from_reason("send stream busy"))?;
        Ok(NapiSendClosed {
            stopped: Arc::new(Mutex::new(Some(Box::pin(stream.stopped())))),
        })
    }

    /// Signal that no more data will be written.
    #[napi]
    pub async fn finish(&self) -> Result<()> {
        let inner = self.inner.clone();
        let mut stream = inner.lock().await;
        stream
            .finish()
            .map_err(|e| Error::from_reason(e.to_string()))
    }

    /// Abruptly reset the stream with an error code.
    #[napi]
    pub async fn reset(&self, code: u32) -> Result<()> {
        let inner = self.inner.clone();
        let mut stream = inner.lock().await;
        stream
            .reset(code)
            .map_err(|e| Error::from_reason(e.to_string()))
    }

    /// Set the priority of the stream.
    #[napi]
    pub async fn set_priority(&self, priority: i32) -> Result<()> {
        let inner = self.inner.clone();
        let stream = inner.lock().await;
        stream
            .set_priority(priority)
            .map_err(|e| Error::from_reason(e.to_string()))
    }
}

type Stopped = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = std::result::Result<Option<u32>, web_transport_quinn::SessionError>,
            > + Send,
    >,
>;

/// A watch on a send stream, holding only the connection rather than the stream.
#[napi]
pub struct NapiSendClosed {
    stopped: Arc<Mutex<Option<Stopped>>>,
}

#[napi]
impl NapiSendClosed {
    /// Wait until the peer has read the whole stream (null), stopped it, or the session failed.
    /// Can only be called once.
    #[napi]
    pub async fn wait(&self) -> Result<Option<NapiStreamError>> {
        // Own the shared state for the duration of the future (see module note).
        let stopped = self.stopped.clone();
        let stopped = stopped
            .lock()
            .await
            .take()
            .ok_or_else(|| Error::from_reason("already waited"))?;
        Ok(match stopped.await {
            Ok(None) => None,
            Ok(Some(code)) => Some(NapiStreamError::stream(
                Some(code),
                format!("STOP_SENDING: {code}"),
            )),
            Err(err) => Some(NapiStreamError::session(&err)),
        })
    }
}

/// A receive stream for reading data.
#[napi]
pub struct NapiRecvStream {
    inner: Arc<Mutex<web_transport_quinn::RecvStream>>,
}

#[napi]
impl NapiRecvStream {
    /// Read up to `max_size` bytes from the stream. Returns null on FIN, or the error if the
    /// peer reset the stream or the session failed.
    #[napi]
    pub async fn read(&self, max_size: u32) -> Result<Option<Either<Buffer, NapiStreamError>>> {
        use web_transport_quinn::ReadError;

        // Own the shared stream for the duration of the future (see module note).
        let inner = self.inner.clone();
        let mut stream = inner.lock().await;
        let err = match stream.read_chunk(max_size as usize, true).await {
            Ok(chunk) => {
                return Ok(chunk.map(|chunk| Either::A(Buffer::from(chunk.bytes.to_vec()))))
            }
            Err(err @ ReadError::Reset(code)) => {
                NapiStreamError::stream(Some(code), err.to_string())
            }
            Err(err @ ReadError::InvalidReset(_)) => NapiStreamError::stream(None, err.to_string()),
            Err(ReadError::SessionError(err)) => NapiStreamError::session(&err),
            Err(err @ (ReadError::ClosedStream | ReadError::IllegalOrderedRead)) => {
                return Err(Error::from_reason(err.to_string()))
            }
        };
        Ok(Some(Either::B(err)))
    }

    /// Tell the peer to stop sending with the given error code.
    #[napi]
    pub async fn stop(&self, code: u32) -> Result<()> {
        let inner = self.inner.clone();
        let mut stream = inner.lock().await;
        stream
            .stop(code)
            .map_err(|e| Error::from_reason(e.to_string()))
    }
}
