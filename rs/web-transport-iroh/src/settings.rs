use iroh::endpoint;
use n0_error::stack_error;
use tokio::try_join;

/// An error during the HTTP/3 SETTINGS frame exchange.
#[stack_error(derive, from_sources)]
#[derive(Clone)]
pub enum SettingsError {
    #[error("quic stream was closed early")]
    UnexpectedEnd,

    #[error("protocol error")]
    ProtoError(#[error(source, from, std_err)] web_transport_proto::SettingsError),

    #[error("WebTransport is not supported")]
    WebTransportUnsupported,

    #[error("connection error")]
    ConnectionError(#[error(source, from, std_err)] endpoint::ConnectionError),

    #[error("read error")]
    ReadError(#[error(source, from, std_err)] endpoint::ReadError),

    #[error("write error")]
    WriteError(#[error(source, from, std_err)] endpoint::WriteError),
}

/// Maintains the HTTP/3 control stream by holding references to the send/recv streams.
#[derive(Debug)]
pub struct Settings {
    // A reference to the send/recv stream, so we don't close it until dropped.
    #[allow(dead_code)]
    send: endpoint::SendStream,

    #[allow(dead_code)]
    recv: endpoint::RecvStream,
}

impl Settings {
    /// Establishes an HTTP/3 connection by exchanging SETTINGS frames.
    pub async fn connect(conn: &endpoint::Connection) -> Result<Self, SettingsError> {
        let recv = Self::accept(conn);
        let send = Self::open(conn);

        // Run both tasks concurrently until one errors or they both complete.
        let (send, recv) = try_join!(send, recv)?;
        Ok(Self { send, recv })
    }

    async fn accept(conn: &endpoint::Connection) -> Result<endpoint::RecvStream, SettingsError> {
        let mut recv = conn.accept_uni().await?;
        let settings = web_transport_proto::Settings::read(&mut recv).await?;

        tracing::debug!("received SETTINGS frame: {settings:?}");

        if settings.supports_webtransport() == 0 {
            return Err(SettingsError::WebTransportUnsupported);
        }

        Ok(recv)
    }

    async fn open(conn: &endpoint::Connection) -> Result<endpoint::SendStream, SettingsError> {
        let mut settings = web_transport_proto::Settings::default();
        settings.enable_webtransport(1);

        tracing::debug!("sending SETTINGS frame: {settings:?}");

        let mut send = conn.open_uni().await?;
        settings.write(&mut send).await?;

        Ok(send)
    }
}
