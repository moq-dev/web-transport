use std::sync::Arc;
use tokio::net::{TcpStream, ToSocketAddrs};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::TlsConnector;

use crate::transport::StreamTransport;
use crate::{alpn, Config, Error, Session, Version};

/// Extract the application protocol from a TLS ALPN, given the pinned version.
///
/// The QMux version is set by the caller; this function only strips the
/// expected prefix to recover the app protocol. Returns `None` for the bare
/// version ALPN or for unrecognised values.
fn parse_protocol(alpn: Option<&str>, version: Version) -> Option<String> {
    let alpn = alpn.filter(|s| !s.is_empty())?;
    if alpn == version.alpn() {
        return None;
    }
    let Some(proto) = alpn.strip_prefix(version.prefix()) else {
        tracing::warn!(?alpn, ?version, "unrecognized TLS ALPN");
        return None;
    };
    (!proto.is_empty()).then(|| proto.to_string())
}

/// Connect over TLS pinned to a specific QMux version.
///
/// The caller's `alpn_protocols` are treated as bare application-level
/// protocols and wrapped with `{version.prefix()}` before offer. The prefix
/// is stripped from the negotiated result. To advertise multiple QMux
/// versions, drive separate connection attempts.
///
/// The `server_name` is used for SNI and certificate verification.
pub async fn connect(
    addr: impl ToSocketAddrs,
    server_name: &str,
    config: Arc<rustls::ClientConfig>,
    version: Version,
) -> Result<Session, Error> {
    let stream = TcpStream::connect(&addr).await?;

    let server_name = rustls::pki_types::ServerName::try_from(server_name)
        .map_err(|e| Error::Io(e.to_string()))?
        .to_owned();

    let app_protocols: Vec<String> = config
        .alpn_protocols
        .iter()
        .map(|a| String::from_utf8_lossy(a).to_string())
        .collect();
    let prefixed = alpn::build(version, &app_protocols);

    let mut config = (*config).clone();
    config.alpn_protocols = prefixed.iter().map(|s| s.as_bytes().to_vec()).collect();

    tracing::debug!(?prefixed, "TLS connecting");

    let connector = TlsConnector::from(Arc::new(config));
    let tls_stream = connector.connect(server_name, stream).await?;

    let negotiated = tls_stream.get_ref().1.alpn_protocol();
    let negotiated_str = negotiated.and_then(|a| std::str::from_utf8(a).ok());
    tracing::debug!(?negotiated_str, "TLS negotiated ALPN");

    let protocol = parse_protocol(negotiated_str, version);
    tracing::debug!(?version, ?protocol, "parsed ALPN");

    let session_config = Config::new(version, protocol);
    let transport = StreamTransport::new(tls_stream, version, session_config.max_record_size);
    Ok(Session::connect(transport, session_config))
}

/// Accept a TLS connection pinned to a specific QMux version.
pub async fn accept(
    stream: TcpStream,
    config: Arc<rustls::ServerConfig>,
    version: Version,
) -> Result<Session, Error> {
    let acceptor = TlsAcceptor::from(config);
    let tls_stream = acceptor.accept(stream).await?;

    let negotiated = tls_stream.get_ref().1.alpn_protocol();
    let negotiated_str = negotiated.and_then(|a| std::str::from_utf8(a).ok());
    tracing::debug!(?negotiated_str, "TLS accepted, negotiated ALPN");

    let protocol = parse_protocol(negotiated_str, version);
    tracing::debug!(?version, ?protocol, "parsed ALPN");

    let session_config = Config::new(version, protocol);
    let transport = StreamTransport::new(tls_stream, version, session_config.max_record_size);
    Ok(Session::accept(transport, session_config))
}
