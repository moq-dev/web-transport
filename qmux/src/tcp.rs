use tokio::net::{TcpStream, ToSocketAddrs};

use crate::transport::StreamTransport;
use crate::{Error, Session, Version};

/// Connect to a TCP server. Always uses the QMux wire format.
pub async fn connect(addr: impl ToSocketAddrs) -> Result<Session, Error> {
    let stream = TcpStream::connect(addr).await?;
    let transport = StreamTransport::new(stream);
    Ok(Session::new(transport, Version::QMux00, false, None))
}

/// Accept a TCP connection. Always uses the QMux wire format.
pub async fn accept(stream: TcpStream) -> Result<Session, Error> {
    let transport = StreamTransport::new(stream);
    Ok(Session::new(transport, Version::QMux00, true, None))
}
