use tokio::net::{TcpStream, ToSocketAddrs};

use crate::transport::StreamTransport;
use crate::{Config, Error, Session, Version};

/// Connect to a TCP server.
///
/// Defaults to `QMux01`. Pass a specific version to pin it.
pub async fn connect(addr: impl ToSocketAddrs, version: Option<Version>) -> Result<Session, Error> {
    let version = version.unwrap_or(Version::QMux01);
    let stream = TcpStream::connect(addr).await?;
    let transport = StreamTransport::new(stream, version);
    Ok(Session::connect(transport, Config::new(version, None)))
}

/// Accept a TCP connection.
///
/// Defaults to `QMux01`. Pass a specific version to pin it.
pub async fn accept(stream: TcpStream, version: Option<Version>) -> Result<Session, Error> {
    let version = version.unwrap_or(Version::QMux01);
    let transport = StreamTransport::new(stream, version);
    Ok(Session::accept(transport, Config::new(version, None)))
}
