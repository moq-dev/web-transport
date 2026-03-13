use std::sync::Arc;
use tokio::net::{TcpStream, ToSocketAddrs};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::TlsConnector;

use crate::transport::StreamTransport;
use crate::{alpn, Error, Session, Version};

/// Parse a TLS ALPN into a version and app protocol.
///
/// Strips the qmux/webtransport prefix (e.g. `qmux-00.moqt-16` → `moqt-16`).
/// TLS always uses the QMux wire format regardless of prefix.
fn parse_alpn(alpn: Option<&str>) -> (Version, Option<String>) {
    let alpn = match alpn {
        Some(s) if !s.is_empty() => s,
        _ => return (Version::QMux00, None),
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

    tracing::warn!(?alpn, "unrecognized TLS ALPN");
    (Version::QMux00, None)
}

/// Connect over TLS. Always uses the QMux wire format.
///
/// The caller's `alpn_protocols` are treated as application-level protocols
/// and automatically wrapped with `qmux-00.` and `webtransport.` prefixes.
/// The prefix is stripped from the negotiated result.
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

    // Convert caller's raw ALPNs to prefixed qmux/webtransport ALPNs.
    let app_protocols: Vec<String> = config
        .alpn_protocols
        .iter()
        .map(|a| String::from_utf8_lossy(a).to_string())
        .collect();
    let prefixed = alpn::build(&app_protocols);

    let mut config = (*config).clone();
    config.alpn_protocols = prefixed.iter().map(|s| s.as_bytes().to_vec()).collect();

    tracing::debug!(?prefixed, "TLS connecting");

    let connector = TlsConnector::from(Arc::new(config));
    let tls_stream = connector.connect(server_name, stream).await?;

    let negotiated = tls_stream.get_ref().1.alpn_protocol();
    let negotiated_str = negotiated.and_then(|a| std::str::from_utf8(a).ok());
    tracing::debug!(?negotiated_str, "TLS negotiated ALPN");

    let (version, protocol) = parse_alpn(negotiated_str);
    tracing::debug!(?version, ?protocol, "parsed ALPN");

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

    let negotiated = tls_stream.get_ref().1.alpn_protocol();
    let negotiated_str = negotiated.and_then(|a| std::str::from_utf8(a).ok());
    tracing::debug!(?negotiated_str, "TLS accepted, negotiated ALPN");

    let (version, protocol) = parse_alpn(negotiated_str);
    tracing::debug!(?version, ?protocol, "parsed ALPN");

    let transport = StreamTransport::new(tls_stream);
    Ok(Session::accept(transport, version, protocol))
}
