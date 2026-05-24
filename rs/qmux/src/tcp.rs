use tokio::net::{TcpStream, ToSocketAddrs};

use crate::transport::StreamTransport;
use crate::{Config, Error, Session, Version};

/// Connect to a TCP server.
///
/// Defaults to `QMux01`. Pass a specific version to pin it.
pub async fn connect(addr: impl ToSocketAddrs, version: Option<Version>) -> Result<Session, Error> {
    let version = version.unwrap_or(Version::QMux01);
    let stream = TcpStream::connect(addr).await?;
    let config = Config::new(version, None);
    let transport = StreamTransport::new(stream, version, config.max_record_size);
    Ok(Session::connect(transport, config))
}

/// Accept a TCP connection.
///
/// Defaults to `QMux01`. Pass a specific version to pin it.
pub async fn accept(stream: TcpStream, version: Option<Version>) -> Result<Session, Error> {
    let version = version.unwrap_or(Version::QMux01);
    let config = Config::new(version, None);
    let transport = StreamTransport::new(stream, version, config.max_record_size);
    Ok(Session::accept(transport, config))
}
