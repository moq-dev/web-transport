use std::sync::Arc;
use tokio::net::{TcpStream, ToSocketAddrs};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::TlsConnector;

use crate::transport::StreamTransport;
use crate::{Error, Session, Version};

/// Parse a TLS ALPN into a version and app protocol.
///
/// Falls back to `QMux00` when the ALPN is missing or unrecognized,
/// since TLS always uses the QMux wire format.
fn parse_alpn(alpn: Option<&[u8]>) -> (Version, Option<String>) {
    let alpn = match alpn {
        Some(a) => std::str::from_utf8(a).unwrap_or(""),
        None => return (Version::QMux00, None),
    };

    for &known in crate::ALPNS {
        if alpn == known {
            return (Version::QMux00, None);
        }
        if let Some(proto) = alpn.strip_prefix(&format!("{known}.")) {
            if !proto.is_empty() {
                return (Version::QMux00, Some(proto.to_string()));
            }
        }
    }

    (Version::QMux00, None)
}

/// Connect over TLS. Always uses the QMux wire format.
///
/// The `server_name` is used for SNI and certificate verification.
/// The ALPN is extracted from the negotiated TLS connection.
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

    let alpn = tls_stream.get_ref().1.alpn_protocol();
    let (version, protocol) = parse_alpn(alpn);

    let transport = StreamTransport::new(tls_stream);
    Ok(Session::connect(transport, version, protocol))
}

/// Accept a TLS connection. Always uses the QMux wire format.
///
/// The ALPN is extracted from the negotiated TLS connection.
pub async fn accept(
    stream: TcpStream,
    config: Arc<rustls::ServerConfig>,
) -> Result<Session, Error> {
    let acceptor = TlsAcceptor::from(config);
    let tls_stream = acceptor.accept(stream).await?;

    let alpn = tls_stream.get_ref().1.alpn_protocol();
    let (version, protocol) = parse_alpn(alpn);

    let transport = StreamTransport::new(tls_stream);
    Ok(Session::accept(transport, version, protocol))
}
