use bytes::{Buf, BufMut, Bytes};
use url::Url;

use web_transport_quinn::http;
// Export the Quinn implementation to simplify Cargo.toml
pub use web_transport_quinn as quinn;

pub use web_transport_quinn::CongestionControl;

/// Create a [Client] that can be used to dial multiple [Session]s.
#[derive(Default, Clone)]
pub struct ClientBuilder {
    inner: quinn::ClientBuilder,
}

impl ClientBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Allow a lower latency congestion controller.
    pub fn with_congestion_control(self, cc: CongestionControl) -> Self {
        Self {
            inner: self.inner.with_congestion_control(cc),
        }
    }

    /// Accept the server's certificate hashes (sha256) instead of using a root CA.
    pub fn with_server_certificate_hashes(self, hashes: Vec<Vec<u8>>) -> Result<Client, Error> {
        Ok(Client {
            inner: self.inner.with_server_certificate_hashes(hashes)?,
        })
    }

    /// Accept certificates using root CAs.
    pub fn with_system_roots(self) -> Result<Client, Error> {
        Ok(Client {
            inner: self.inner.with_system_roots()?,
        })
    }
}

/// Used to dial multiple [Session]s.
#[derive(Clone, Debug)]
pub struct Client {
    inner: quinn::Client,
}

impl Client {
    /// Connect to the server.
    pub async fn connect(&self, url: Url) -> Result<Session, Error> {
        Ok(self.inner.connect(url).await?.into())
    }
}

/// Used to accept incoming connections and create [Session]s. (native only)
///
/// NOTE: This is not supported in the WASM runtime, as browsers are clients.
///
/// Use a [web_transport_quinn::ServerBuilder] to create a [web_transport_quinn::Server] and then [Into<Server>].
/// Alternatively, establish a [web_transport_quinn::Session] directly and then [Into<Session>].
pub struct Server {
    inner: quinn::Server,
}

impl From<quinn::Server> for Server {
    fn from(server: quinn::Server) -> Self {
        Self { inner: server }
    }
}

impl Server {
    /// Accept an incoming connection.
    pub async fn accept(&mut self) -> Result<Option<Session>, Error> {
        match self.inner.accept().await {
            // TODO add sub-protocol support
            Some(session) => Ok(Some(session.ok(http::StatusCode::OK).await?.into())),
            None => Ok(None),
        }
    }
}

/// A WebTransport Session, able to accept/create streams and send/recv datagrams.
///
/// The session can be cloned to create multiple handles, which is which no method is &mut.
/// The session will be closed with on drop.
#[derive(Clone, PartialEq, Eq)]
pub struct Session {
    inner: quinn::Session,
}

impl Session {
    /// Block until the peer creates a new unidirectional stream.
    ///
    /// Won't return None unless the connection is closed.
    pub async fn accept_uni(&self) -> Result<RecvStream, Error> {
        let stream = self.inner.accept_uni().await?;
        Ok(RecvStream::new(stream))
    }

    /// Block until the peer creates a new bidirectional stream.
    pub async fn accept_bi(&self) -> Result<(SendStream, RecvStream), Error> {
        let (s, r) = self.inner.accept_bi().await?;
        Ok((SendStream::new(s), RecvStream::new(r)))
    }

    /// Open a new bidirectional stream, which may block when there are too many concurrent streams.
    pub async fn open_bi(&self) -> Result<(SendStream, RecvStream), Error> {
        Ok(self
            .inner
            .open_bi()
            .await
            .map(|(s, r)| (SendStream::new(s), RecvStream::new(r)))?)
    }

    /// Open a new unidirectional stream, which may block when there are too many concurrent streams.
    pub async fn open_uni(&self) -> Result<SendStream, Error> {
        Ok(self.inner.open_uni().await.map(SendStream::new)?)
    }

    /// Send a datagram over the network.
    ///
    /// QUIC datagrams may be dropped for any reason:
    /// - Network congestion.
    /// - Random packet loss.
    /// - Payload is larger than `max_datagram_size()`
    /// - Peer is not receiving datagrams.
    /// - Peer has too many outstanding datagrams.
    /// - ???
    pub async fn send_datagram(&self, payload: Bytes) -> Result<(), Error> {
        // NOTE: This is not async, but we need to make it async to match the wasm implementation.
        Ok(self.inner.send_datagram(payload)?)
    }

    /// The maximum size of a datagram that can be sent.
    pub async fn max_datagram_size(&self) -> usize {
        self.inner.max_datagram_size()
    }

    /// Receive a datagram over the network.
    pub async fn recv_datagram(&self) -> Result<Bytes, Error> {
        Ok(self.inner.read_datagram().await?)
    }

    /// Close the connection immediately with a code and reason.
    pub fn close(&self, code: u32, reason: &str) {
        self.inner.close(code, reason.as_bytes())
    }

    /// Block until the connection is closed.
    pub async fn closed(&self) -> Error {
        self.inner.closed().await.into()
    }

    /// Return the URL used to create the session.
    pub fn url(&self) -> &Url {
        &self.inner.request().url
    }

    /// Return the application protocol used to create the session.
    pub fn protocol(&self) -> Option<&str> {
        self.inner.response().protocol.as_deref()
    }
}

/// Convert a `web_transport_quinn::Session` into a `web_transport::Session`.
impl From<quinn::Session> for Session {
    fn from(session: quinn::Session) -> Self {
        Session { inner: session }
    }
}

/// An outgoing stream of bytes to the peer.
///
/// QUIC streams have flow control, which means the send rate is limited by the peer's receive window.
/// The stream will be closed with a graceful FIN when dropped.
pub struct SendStream {
    inner: quinn::SendStream,
}

impl SendStream {
    fn new(inner: quinn::SendStream) -> Self {
        Self { inner }
    }

    /// Write some of the buffer to the stream.
    #[must_use = "returns the number of bytes written"]
    pub async fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
        self.inner.write(buf).await.map_err(Into::into)
    }

    /// Write some of the buffer to the stream, advancing the internal position.
    pub async fn write_buf<B: Buf>(&mut self, buf: &mut B) -> Result<usize, Error> {
        // We use copy_to_bytes+write_chunk so if Bytes is provided, we can avoid allocating.
        let size = buf.chunk().len();
        let chunk = buf.copy_to_bytes(size);
        self.inner.write_chunk(chunk).await?;
        Ok(size)
    }

    /// Set the stream's priority.
    ///
    /// Streams with lower values will be sent first, but are not guaranteed to arrive first.
    pub fn set_priority(&mut self, order: i32) {
        self.inner.set_priority(order).ok();
    }

    /// Send an immediate reset code, closing the stream.
    pub fn reset(&mut self, code: u32) {
        self.inner.reset(code).ok();
    }

    /// Mark the stream as finished.
    ///
    /// This is automatically called on Drop, but can be called manually.
    pub fn finish(&mut self) -> Result<(), Error> {
        self.inner
            .finish()
            .map_err(|_| Error::Write(quinn::WriteError::ClosedStream))?;
        Ok(())
    }

    /// Block until the stream is closed by either side.
    ///
    /// This returns a (potentially truncated) u8 because that's what the WASM implementation returns.
    // TODO this should be &self but requires modifying quinn.
    pub async fn closed(&mut self) -> Result<Option<u8>, Error> {
        match self.inner.stopped().await {
            Ok(None) => Ok(None),
            Ok(Some(code)) => Ok(Some(code as u8)),
            Err(e) => Err(Error::Session(e)),
        }
    }
}

/// An incoming stream of bytes from the peer.
///
/// All bytes are flushed in order and the stream is flow controlled.
/// The stream will be closed with STOP_SENDING code=0 when dropped.
pub struct RecvStream {
    inner: quinn::RecvStream,
}

impl RecvStream {
    fn new(inner: quinn::RecvStream) -> Self {
        Self { inner }
    }

    /// Read the next chunk of data with the provided maximum size.
    ///
    /// This returns a chunk of data instead of copying, which may be more efficient.
    pub async fn read(&mut self, max: usize) -> Result<Option<Bytes>, Error> {
        Ok(self
            .inner
            .read_chunk(max, true)
            .await?
            .map(|chunk| chunk.bytes))
    }

    /// Read some data into the provided buffer.
    ///
    /// The number of bytes read is returned, or None if the stream is closed.
    /// The buffer will be advanced by the number of bytes read.
    pub async fn read_buf<B: BufMut>(&mut self, buf: &mut B) -> Result<Option<usize>, Error> {
        let dst = buf.chunk_mut();
        let dst = unsafe { &mut *(dst as *mut _ as *mut [u8]) };

        let size = match self.inner.read(dst).await? {
            Some(size) if size > 0 => size,
            _ => return Ok(None),
        };

        unsafe { buf.advance_mut(size) };

        Ok(Some(size))
    }

    /// Send a `STOP_SENDING` QUIC code.
    pub fn stop(&mut self, code: u32) {
        self.inner.stop(code).ok();
    }

    /// Block until the stream has been closed and return the error code, if any.
    ///
    /// This returns a (potentially truncated) u8 because that's what the WASM implementation returns.
    /// web-transport-quinn returns a u32 because that's what the specification says.
    // TODO Validate the correct behavior.
    pub async fn closed(&mut self) -> Result<Option<u8>, Error> {
        match self.inner.received_reset().await {
            Ok(None) => Ok(None),
            Ok(Some(code)) => Ok(Some(code as u8)),
            Err(e) => Err(Error::Session(e)),
        }
    }
}

/// A WebTransport error.
///
/// The source can either be a session error or a stream error.
/// TODO This interface is currently not generic.
#[derive(Debug, thiserror::Error, Clone)]
pub enum Error {
    #[error("session error: {0}")]
    Session(#[from] quinn::SessionError),

    #[error("server error: {0}")]
    Server(#[from] quinn::ServerError),

    #[error("client error: {0}")]
    Client(#[from] quinn::ClientError),

    #[error("write error: {0}")]
    Write(quinn::WriteError),

    #[error("read error: {0}")]
    Read(quinn::ReadError),
}

impl From<quinn::WriteError> for Error {
    fn from(e: quinn::WriteError) -> Self {
        match e {
            quinn::WriteError::SessionError(e) => Error::Session(e),
            e => Error::Write(e),
        }
    }
}
impl From<quinn::ReadError> for Error {
    fn from(e: quinn::ReadError) -> Self {
        match e {
            quinn::ReadError::SessionError(e) => Error::Session(e),
            e => Error::Read(e),
        }
    }
}
