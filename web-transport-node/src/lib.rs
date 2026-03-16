use napi::bindgen_prelude::*;
use napi_derive::napi;

use tokio::sync::Mutex;

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
        let client = web_transport_quinn::ClientBuilder::new()
            .with_system_roots()
            .map_err(|e| Error::from_reason(e.to_string()))?;
        Ok(Self { inner: client })
    }

    /// Create a client that validates server certificates by SHA-256 hash.
    #[napi(factory)]
    pub fn with_certificate_hashes(hashes: Vec<Buffer>) -> Result<Self> {
        let hashes: Vec<Vec<u8>> = hashes.into_iter().map(|b| b.to_vec()).collect();
        let client = web_transport_quinn::ClientBuilder::new()
            .with_server_certificate_hashes(hashes)
            .map_err(|e| Error::from_reason(e.to_string()))?;
        Ok(Self { inner: client })
    }

    /// Connect to a WebTransport server at the given URL.
    #[napi]
    pub async fn connect(&self, url_str: String) -> Result<NapiSession> {
        let url: url::Url = url_str
            .parse()
            .map_err(|e: url::ParseError| Error::from_reason(e.to_string()))?;
        let session = self
            .inner
            .connect(url)
            .await
            .map_err(|e| Error::from_reason(e.to_string()))?;
        Ok(NapiSession {
            inner: session.clone(),
            closed: Mutex::new(None),
        })
    }
}

/// A WebTransport server that accepts incoming sessions.
#[napi]
pub struct NapiServer {
    inner: Mutex<web_transport_quinn::Server>,
}

#[napi]
impl NapiServer {
    /// Create a server bound to the given address with the given TLS certificate.
    #[napi(factory)]
    pub fn bind(addr: String, cert_pem: Buffer, key_pem: Buffer) -> Result<Self> {
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
            inner: Mutex::new(server),
        })
    }

    /// Accept the next incoming WebTransport session request.
    #[napi]
    pub async fn accept(&self) -> Result<Option<NapiRequest>> {
        let mut server = self.inner.lock().await;
        match server.accept().await {
            Some(request) => Ok(Some(NapiRequest {
                inner: Mutex::new(Some(request)),
            })),
            None => Ok(None),
        }
    }
}

/// A pending WebTransport session request from a client.
#[napi]
pub struct NapiRequest {
    // Option so we can take it in ok()/reject() which consume the Request.
    inner: Mutex<Option<web_transport_quinn::Request>>,
}

#[napi]
impl NapiRequest {
    /// Get the URL of the CONNECT request.
    #[napi(getter)]
    pub async fn url(&self) -> Result<String> {
        let guard = self.inner.lock().await;
        let request = guard
            .as_ref()
            .ok_or_else(|| Error::from_reason("request already consumed"))?;
        Ok(request.url.to_string())
    }

    /// Accept the session with 200 OK.
    #[napi]
    pub async fn ok(&self) -> Result<NapiSession> {
        let request = self
            .inner
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
            closed: Mutex::new(None),
        })
    }

    /// Reject the session with the given HTTP status code.
    #[napi]
    pub async fn reject(&self, status: u16) -> Result<()> {
        let request = self
            .inner
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
    closed: Mutex<Option<String>>,
}

/// Info about why a session was closed.
#[napi(object)]
pub struct NapiCloseInfo {
    pub code: u32,
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
    /// Accept an incoming unidirectional stream.
    #[napi]
    pub async fn accept_uni(&self) -> Result<NapiRecvStream> {
        let recv = self
            .inner
            .accept_uni()
            .await
            .map_err(|e| Error::from_reason(e.to_string()))?;
        Ok(NapiRecvStream {
            inner: Mutex::new(recv),
        })
    }

    /// Accept an incoming bidirectional stream.
    #[napi]
    pub async fn accept_bi(&self) -> Result<NapiBiStream> {
        let (send, recv) = self
            .inner
            .accept_bi()
            .await
            .map_err(|e| Error::from_reason(e.to_string()))?;
        Ok(NapiBiStream {
            send: Some(NapiSendStream {
                inner: Mutex::new(send),
            }),
            recv: Some(NapiRecvStream {
                inner: Mutex::new(recv),
            }),
        })
    }

    /// Open a new unidirectional stream.
    #[napi]
    pub async fn open_uni(&self) -> Result<NapiSendStream> {
        let send = self
            .inner
            .open_uni()
            .await
            .map_err(|e| Error::from_reason(e.to_string()))?;
        Ok(NapiSendStream {
            inner: Mutex::new(send),
        })
    }

    /// Open a new bidirectional stream.
    #[napi]
    pub async fn open_bi(&self) -> Result<NapiBiStream> {
        let (send, recv) = self
            .inner
            .open_bi()
            .await
            .map_err(|e| Error::from_reason(e.to_string()))?;
        Ok(NapiBiStream {
            send: Some(NapiSendStream {
                inner: Mutex::new(send),
            }),
            recv: Some(NapiRecvStream {
                inner: Mutex::new(recv),
            }),
        })
    }

    /// Send a datagram.
    #[napi]
    pub fn send_datagram(&self, data: Buffer) -> Result<()> {
        self.inner
            .send_datagram(bytes::Bytes::from(data.to_vec()))
            .map_err(|e| Error::from_reason(e.to_string()))
    }

    /// Receive a datagram.
    #[napi]
    pub async fn recv_datagram(&self) -> Result<Buffer> {
        let data = self
            .inner
            .read_datagram()
            .await
            .map_err(|e| Error::from_reason(e.to_string()))?;
        Ok(Buffer::from(data.to_vec()))
    }

    /// Get the maximum datagram size.
    #[napi]
    pub fn max_datagram_size(&self) -> u32 {
        self.inner.max_datagram_size() as u32
    }

    /// Close the session with a code and reason.
    #[napi]
    pub fn close(&self, code: u32, reason: String) {
        self.inner.close(code, reason.as_bytes());
    }

    /// Wait for the session to close, returning the error reason.
    #[napi]
    pub async fn closed(&self) -> Result<String> {
        // Check if we already have a cached result.
        {
            let cached = self.closed.lock().await;
            if let Some(reason) = cached.as_ref() {
                return Ok(reason.clone());
            }
        }

        let err = self.inner.closed().await;
        let reason = err.to_string();

        // Cache the result.
        {
            let mut cached = self.closed.lock().await;
            *cached = Some(reason.clone());
        }

        Ok(reason)
    }
}

/// A send stream for writing data.
#[napi]
pub struct NapiSendStream {
    inner: Mutex<web_transport_quinn::SendStream>,
}

#[napi]
impl NapiSendStream {
    /// Write data to the stream.
    #[napi]
    pub async fn write(&self, data: Buffer) -> Result<()> {
        let mut stream = self.inner.lock().await;
        stream
            .write_all(&data)
            .await
            .map_err(|e| Error::from_reason(e.to_string()))
    }

    /// Signal that no more data will be written.
    #[napi]
    pub async fn finish(&self) -> Result<()> {
        let mut stream = self.inner.lock().await;
        stream
            .finish()
            .map_err(|e| Error::from_reason(e.to_string()))
    }

    /// Abruptly reset the stream with an error code.
    #[napi]
    pub async fn reset(&self, code: u32) -> Result<()> {
        let mut stream = self.inner.lock().await;
        stream
            .reset(code)
            .map_err(|e| Error::from_reason(e.to_string()))
    }

    /// Set the priority of the stream.
    #[napi]
    pub async fn set_priority(&self, priority: i32) -> Result<()> {
        let stream = self.inner.lock().await;
        stream
            .set_priority(priority)
            .map_err(|e| Error::from_reason(e.to_string()))
    }
}

/// A receive stream for reading data.
#[napi]
pub struct NapiRecvStream {
    inner: Mutex<web_transport_quinn::RecvStream>,
}

#[napi]
impl NapiRecvStream {
    /// Read up to `max_size` bytes from the stream. Returns null on FIN.
    #[napi]
    pub async fn read(&self, max_size: u32) -> Result<Option<Buffer>> {
        let mut stream = self.inner.lock().await;
        let mut buf = vec![0u8; max_size as usize];
        match stream
            .read(&mut buf)
            .await
            .map_err(|e| Error::from_reason(e.to_string()))?
        {
            Some(n) => {
                buf.truncate(n);
                Ok(Some(Buffer::from(buf)))
            }
            None => Ok(None),
        }
    }

    /// Tell the peer to stop sending with the given error code.
    #[napi]
    pub async fn stop(&self, code: u32) -> Result<()> {
        let mut stream = self.inner.lock().await;
        stream
            .stop(code)
            .map_err(|e| Error::from_reason(e.to_string()))
    }
}
