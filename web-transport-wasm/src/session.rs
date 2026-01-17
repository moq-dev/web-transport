use bytes::Bytes;
use js_sys::{Reflect, Uint8Array};
use url::Url;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    WebTransport, WebTransportBidirectionalStream, WebTransportCloseInfo, WebTransportSendStream,
};

use crate::{Error, RecvStream, SendStream};
use web_streams::{Reader, Writer};

/// A session represents a connection between a client and a server.
///
/// This is the main entry point for creating new streams and sending datagrams.
/// The session can be closed by either endpoint with an error code and reason.
///
/// The session can be cloned to create multiple handles.
/// However, handles cannot (currently) accept/open the same type of stream.
#[derive(Clone)]
pub struct Session {
    inner: WebTransport,
    url: Url,
    protocol: Option<String>,
}

impl Session {
    pub fn new(inner: WebTransport, url: Url) -> Self {
        // TODO use the web_sys bindings when updated.
        // Until then, we try to access the protocol property on the inner object.
        let protocol = Reflect::get(&inner, &"protocol".into())
            .ok()
            .and_then(|p| p.as_string());

        Self {
            inner,
            url,
            protocol,
        }
    }

    /// Accept a new unidirectional stream from the peer.
    pub async fn accept_uni(&self) -> Result<RecvStream, Error> {
        let mut reader = Reader::new(&self.inner.incoming_unidirectional_streams())?;

        match reader.read().await? {
            Some(stream) => Ok(RecvStream::new(stream)?),
            None => Err(self.closed().await),
        }
    }

    /// Accept a new bidirectional stream from the peer.
    pub async fn accept_bi(&self) -> Result<(SendStream, RecvStream), Error> {
        let mut reader = Reader::new(&self.inner.incoming_bidirectional_streams())?;

        let stream: WebTransportBidirectionalStream = match reader.read().await? {
            Some(stream) => stream,
            None => return Err(self.closed().await),
        };

        let send = SendStream::new(stream.writable())?;
        let recv = RecvStream::new(stream.readable())?;

        Ok((send, recv))
    }

    /// Creates a new bidirectional stream.
    pub async fn open_bi(&self) -> Result<(SendStream, RecvStream), Error> {
        let stream: WebTransportBidirectionalStream =
            JsFuture::from(self.inner.create_bidirectional_stream())
                .await?
                .into();

        let send = SendStream::new(stream.writable())?;
        let recv = RecvStream::new(stream.readable())?;

        Ok((send, recv))
    }

    /// Creates a new unidirectional stream.
    pub async fn open_uni(&self) -> Result<SendStream, Error> {
        let stream: WebTransportSendStream =
            JsFuture::from(self.inner.create_unidirectional_stream())
                .await?
                .into();

        let send = SendStream::new(stream)?;
        Ok(send)
    }

    /// Send a datagram over the network.
    pub async fn send_datagram(&self, payload: Bytes) -> Result<(), Error> {
        let mut writer = Writer::new(&self.inner.datagrams().writable())?;
        writer.write(&Uint8Array::from(payload.as_ref())).await?;
        Ok(())
    }

    /// Receive a datagram over the network.
    pub async fn recv_datagram(&self) -> Result<Bytes, Error> {
        let mut reader = Reader::new(&self.inner.datagrams().readable())?;
        let data: Uint8Array = reader.read().await?.unwrap_or_default();
        Ok(data.to_vec().into())
    }

    /// Close the session with the given error code and reason.
    pub fn close(&self, code: u32, reason: &str) {
        let info = WebTransportCloseInfo::new();
        info.set_close_code(code);
        info.set_reason(reason);
        self.inner.close_with_close_info(&info);
    }

    /// Block until the session is closed and return the error.
    pub async fn closed(&self) -> Error {
        self.closed_inner().await.unwrap_err()
    }

    async fn closed_inner(&self) -> Result<(), Error> {
        let info: WebTransportCloseInfo = JsFuture::from(self.inner.closed()).await?.into();
        let reason = info.get_reason().unwrap_or_default();

        let options = web_sys::WebTransportErrorOptions::new();
        options.set_source(web_sys::WebTransportErrorSource::Session);

        if let Ok(code) = info.get_close_code().map(u8::try_from).transpose() {
            options.set_stream_error_code(code);
        }

        let err = web_sys::WebTransportError::new_with_message_and_options(&reason, &options)?;
        Err(Error::Session(err))
    }

    /// Return the URL used to create the session.
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// Return the application protocol used to create the session.
    pub fn protocol(&self) -> Option<&str> {
        self.protocol.as_deref()
    }
}

impl PartialEq for Session {
    fn eq(&self, other: &Self) -> bool {
        self.inner == other.inner
    }
}

impl Eq for Session {}
