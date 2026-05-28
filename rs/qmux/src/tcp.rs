use tokio::net::{TcpStream, ToSocketAddrs};

use crate::transport::StreamTransport;
use crate::{Config, Error, Session, Version};

/// Connect to a TCP server and speak QMux at `version`.
///
/// TCP has no protocol negotiation, so the version is a direct parameter
/// instead of derived from an ALPN.
pub async fn connect(addr: impl ToSocketAddrs, version: Version) -> Result<Session, Error> {
    let stream = TcpStream::connect(addr).await?;
    let config = Config::new(version, None);
    let transport = StreamTransport::new(stream, version, config.max_record_size);
    Ok(Session::connect(transport, config))
}

/// Accept a TCP connection and speak QMux at `version`.
pub async fn accept(stream: TcpStream, version: Version) -> Result<Session, Error> {
    let config = Config::new(version, None);
    let transport = StreamTransport::new(stream, version, config.max_record_size);
    Ok(Session::accept(transport, config))
}
