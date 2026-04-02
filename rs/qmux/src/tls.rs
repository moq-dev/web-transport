use std::sync::Arc;
use tokio::net::{TcpStream, ToSocketAddrs};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::TlsConnector;

use crate::transport::StreamTransport;
use crate::{alpn, Config, Error, Session, Version};

/// Parse a TLS ALPN into a version and app protocol.
///
/// Strips the qmux/webtransport prefix (e.g. `qmux-01.moqt-16` → `moqt-16`).
/// If `pin` is `Some`, the version is forced regardless of the ALPN prefix.
fn parse_alpn(alpn: Option<&str>, pin: Option<Version>) -> (Version, Option<String>) {
    let alpn = match alpn {
        Some(s) if !s.is_empty() => s,
        _ => return (pin.unwrap_or(Version::QMux01), None),
    };

    // Try each version in preference order
    for &version in Version::ALL {
        if alpn == version.alpn() {
            return (pin.unwrap_or(version), None);
        }
        if let Some(proto) = alpn.strip_prefix(version.prefix()) {
            let proto = if proto.is_empty() {
                None
            } else {
                Some(proto.to_string())
            };
            return (pin.unwrap_or(version), proto);
        }
    }

    tracing::warn!(?alpn, "unrecognized TLS ALPN");
    (pin.unwrap_or(Version::QMux01), None)
}

/// Connect over TLS.
///
/// The caller's `alpn_protocols` are treated as application-level protocols
/// and automatically wrapped with version-specific prefixes.
/// The prefix is stripped from the negotiated result.
///
/// If `version` is `Some`, only that QMux version is offered and used,
/// regardless of the negotiated ALPN prefix. This is useful when an
/// application protocol mandates a specific QMux version (e.g. moq-transport-17
/// always uses `QMux00`).
///
/// The `server_name` is used for SNI and certificate verification.
pub async fn connect(
    addr: impl ToSocketAddrs,
    server_name: &str,
    config: Arc<rustls::ClientConfig>,
    version: Option<Version>,
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
    let versions: Vec<Version> = version.into_iter().collect();
    let prefixed = alpn::build(&app_protocols, &versions);

    let mut config = (*config).clone();
    config.alpn_protocols = prefixed.iter().map(|s| s.as_bytes().to_vec()).collect();

    tracing::debug!(?prefixed, "TLS connecting");

    let connector = TlsConnector::from(Arc::new(config));
    let tls_stream = connector.connect(server_name, stream).await?;

    let negotiated = tls_stream.get_ref().1.alpn_protocol();
    let negotiated_str = negotiated.and_then(|a| std::str::from_utf8(a).ok());
    tracing::debug!(?negotiated_str, "TLS negotiated ALPN");

    let (version, protocol) = parse_alpn(negotiated_str, version);
    tracing::debug!(?version, ?protocol, "parsed ALPN");

    let transport = StreamTransport::new(tls_stream, version);
    Ok(Session::connect(transport, Config::new(version, protocol)))
}

/// Accept a TLS connection.
///
/// If `version` is `Some`, the version is forced regardless of the
/// negotiated ALPN prefix. The ALPN is still used to extract the
/// application-level protocol name.
pub async fn accept(
    stream: TcpStream,
    config: Arc<rustls::ServerConfig>,
    version: Option<Version>,
) -> Result<Session, Error> {
    let acceptor = TlsAcceptor::from(config);
    let tls_stream = acceptor.accept(stream).await?;

    let negotiated = tls_stream.get_ref().1.alpn_protocol();
    let negotiated_str = negotiated.and_then(|a| std::str::from_utf8(a).ok());
    tracing::debug!(?negotiated_str, "TLS accepted, negotiated ALPN");

    let (version, protocol) = parse_alpn(negotiated_str, version);
    tracing::debug!(?version, ?protocol, "parsed ALPN");

    let transport = StreamTransport::new(tls_stream, version);
    Ok(Session::accept(transport, Config::new(version, protocol)))
}
