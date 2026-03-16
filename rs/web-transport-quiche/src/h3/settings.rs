use futures::try_join;

use thiserror::Error;

use crate::ez;

/// An error returned when exchanging HTTP/3 SETTINGS frames.
#[derive(Error, Debug, Clone)]
pub enum SettingsError {
    #[error("quic stream was closed early")]
    UnexpectedEnd,

    #[error("protocol error: {0}")]
    Proto(#[from] web_transport_proto::SettingsError),

    #[error("WebTransport is not supported")]
    WebTransportUnsupported,

    #[error("connection error")]
    Connection(#[from] ez::ConnectionError),

    #[error("stream error: {0}")]
    Stream(#[from] ez::StreamError),
}

/// HTTP/3 SETTINGS frame exchange for WebTransport support negotiation.
pub struct Settings {
    // A reference to the send/recv stream, so we don't close it until dropped.
    #[allow(dead_code)]
    send: ez::SendStream,

    #[allow(dead_code)]
    recv: ez::RecvStream,
}

impl Settings {
    /// Exchange HTTP/3 SETTINGS frames to negotiate WebTransport support.
    ///
    /// This sends and receives SETTINGS frames to ensure both sides support WebTransport.
    pub async fn connect(conn: &ez::Connection) -> Result<Self, SettingsError> {
        let recv = Self::accept(conn);
        let send = Self::open(conn);

        // Run both tasks concurrently until one errors or they both complete.
        let (send, recv) = try_join!(send, recv)?;
        Ok(Self { send, recv })
    }

    async fn accept(conn: &ez::Connection) -> Result<ez::RecvStream, SettingsError> {
        let mut recv = conn.accept_uni().await?;
        let settings = web_transport_proto::Settings::read(&mut recv).await?;

        tracing::debug!("received SETTINGS frame: {settings:?}");

        if settings.supports_webtransport() == 0 {
            return Err(SettingsError::WebTransportUnsupported);
        }

        Ok(recv)
    }

    async fn open(conn: &ez::Connection) -> Result<ez::SendStream, SettingsError> {
        let mut settings = web_transport_proto::Settings::default();
        settings.enable_webtransport(1);

        tracing::debug!("sending SETTINGS frame: {settings:?}");

        let mut send = conn.open_uni().await?;
        settings.write(&mut send).await?;

        Ok(send)
    }
}
