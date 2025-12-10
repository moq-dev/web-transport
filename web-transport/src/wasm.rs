use bytes::{Buf, BufMut, Bytes};
use url::Url;

pub use web_transport_wasm::CongestionControl;

// Export the Wasm implementation to simplify Cargo.toml
pub use web_transport_wasm as wasm;

pub struct ClientBuilder {
    inner: web_transport_wasm::ClientBuilder,
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ClientBuilder {
    pub fn new() -> Self {
        Self {
            inner: web_transport_wasm::ClientBuilder::new(),
        }
    }

    pub fn with_unreliable(self, val: bool) -> Self {
        Self {
            inner: self.inner.with_unreliable(val),
        }
    }

    pub fn with_congestion_control(self, cc: CongestionControl) -> Self {
        Self {
            inner: self.inner.with_congestion_control(cc),
        }
    }

    pub fn with_server_certificate_hashes(self, hashes: Vec<Vec<u8>>) -> Result<Client, Error> {
        Ok(Client {
            inner: self.inner.with_server_certificate_hashes(hashes),
        })
    }

    pub fn with_system_roots(self) -> Result<Client, Error> {
        Ok(Client {
            inner: self.inner.with_system_roots(),
        })
    }
}

#[derive(Clone, Debug)]
pub struct Client {
    inner: web_transport_wasm::Client,
}

impl Client {
    pub async fn connect(&self, url: Url) -> Result<Session, Error> {
        Ok(self.inner.connect(url).await?.into())
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Session(web_transport_wasm::Session);

impl Session {
    pub async fn accept_uni(&self) -> Result<RecvStream, Error> {
        let stream = self.0.accept_uni().await?;
        Ok(RecvStream(stream))
    }

    pub async fn accept_bi(&self) -> Result<(SendStream, RecvStream), Error> {
        let (s, r) = self.0.accept_bi().await?;
        Ok((SendStream(s), RecvStream(r)))
    }

    pub async fn open_bi(&self) -> Result<(SendStream, RecvStream), Error> {
        let (s, r) = self.0.open_bi().await?;
        Ok((SendStream(s), RecvStream(r)))
    }

    pub async fn open_uni(&self) -> Result<SendStream, Error> {
        self.0.open_uni().await.map(SendStream)
    }

    /// Close the connection immediately
    pub fn close(&self, code: u32, reason: &str) {
        self.0.close(code, reason)
    }

    pub async fn closed(&self) -> Error {
        self.0.closed().await
    }

    /// Send a datagram.
    pub async fn send_datagram(&self, payload: Bytes) -> Result<(), Error> {
        self.0.send_datagram(payload).await
    }

    pub async fn recv_datagram(&self) -> Result<Bytes, Error> {
        self.0.recv_datagram().await
    }

    /// Return the URL used to create the session.
    pub fn url(&self) -> &Url {
        self.0.url()
    }
}

impl From<web_transport_wasm::Session> for Session {
    fn from(session: web_transport_wasm::Session) -> Self {
        Session(session)
    }
}

pub struct SendStream(web_transport_wasm::SendStream);

impl SendStream {
    /// Write some of the given data to the stream.
    pub async fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
        // NOTE: web-transport-wasm writes the entire buffer.
        self.0.write(buf).await?;
        Ok(buf.len())
    }

    /// Write some of the given buffer to the stream.
    pub async fn write_buf<B: Buf>(&mut self, buf: &mut B) -> Result<usize, Error> {
        self.0.write_buf(buf).await
    }

    pub fn set_priority(&mut self, order: i32) {
        self.0.set_priority(order)
    }

    /// Send a QUIC reset code.
    pub fn reset(&mut self, code: u32) {
        self.0.reset(&code.to_string())
    }

    /// Mark the stream as finished.
    ///
    /// This is automatically called on Drop, but can be called manually.
    pub fn finish(&mut self) -> Result<(), Error> {
        self.0.finish()?;
        Ok(())
    }

    /// Block until the stream has been closed and return the error code, if any.
    pub async fn closed(&mut self) -> Result<Option<u8>, Error> {
        self.0.closed().await
    }
}

pub struct RecvStream(web_transport_wasm::RecvStream);

impl RecvStream {
    /// Attempt to read a chunk of unbuffered data.
    pub async fn read(&mut self, max: usize) -> Result<Option<Bytes>, Error> {
        self.0.read(max).await
    }

    /// Attempt to read from the stream into the given buffer.
    pub async fn read_buf<B: BufMut>(&mut self, buf: &mut B) -> Result<Option<usize>, Error> {
        self.0.read_buf(buf).await
    }

    /// Send a `STOP_SENDING` QUIC code.
    pub fn stop(&mut self, code: u32) {
        self.0.stop(&code.to_string())
    }

    /// Block until the stream has been closed and return the error code, if any.
    pub async fn closed(&mut self) -> Result<Option<u8>, Error> {
        self.0.closed().await
    }
}

pub type Error = web_transport_wasm::Error;
