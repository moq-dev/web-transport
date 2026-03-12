use std::sync::Arc;
use tokio::net::{TcpStream, ToSocketAddrs};
use tokio_rustls::{TlsAcceptor, TlsConnector};

use crate::transport::StreamTransport;
use crate::{Error, Session, Version};

/// Connect over TLS. Always uses the QMux wire format.
///
/// The `server_name` is used for SNI and certificate verification.
pub async fn connect(
    addr: impl ToSocketAddrs,
    server_name: &str,
    config: Arc<rustls::ClientConfig>,
) -> Result<Session, Error> {
    let stream = TcpStream::connect(&addr).await?;

    let server_name = rustls::pki_types::ServerName::try_from(server_name)
        .map_err(|e| Error::Io(e.to_string()))?
        .to_owned();

    let connector = TlsConnector::from(config);
    let tls_stream = connector.connect(server_name, stream).await?;

    let transport = StreamTransport::new(tls_stream);
    Ok(Session::connect(transport, Version::QMux00, None))
}

/// Accept a TLS connection. Always uses the QMux wire format.
pub async fn accept(
    stream: TcpStream,
    config: Arc<rustls::ServerConfig>,
) -> Result<Session, Error> {
    let acceptor = TlsAcceptor::from(config);
    let tls_stream = acceptor.accept(stream).await?;

    let transport = StreamTransport::new(tls_stream);
    Ok(Session::accept(transport, Version::QMux00, None))
}
