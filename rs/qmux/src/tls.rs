use std::sync::Arc;
use tokio::net::{TcpStream, ToSocketAddrs};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::TlsConnector;

use crate::transport::Stream;
use crate::{alpn, Config, Error, Session, Version};

/// Connect over TLS, advertising the given `(alpn, versions)` entries.
///
/// Each entry's `versions` slice is expanded into the TLS ALPN list as
/// `{v.prefix()}{alpn}` per version (empty = every QMux draft this crate
/// knows about). The bare version ALPNs (`qmux-01`, `qmux-00`, `webtransport`)
/// are appended as well unless `require_protocol` is set, in which case only
/// the prefixed pairs are offered. The peer's chosen ALPN determines the QMux
/// wire-format version; the `alpn_protocols` field on the supplied
/// `rustls::ClientConfig` is ignored and rebuilt from `entries`.
///
/// `server_name` is used for SNI and certificate verification.
///
/// `path` is an optional requested resource path. TLS has no request line of
/// its own, so when set it is sent in-band via the QMux `path` transport
/// parameter; the server reads it via [`Session::peer_path`]. Pass `None` to
/// send no path.
pub async fn connect<'a>(
    addr: impl ToSocketAddrs,
    server_name: &str,
    config: Arc<rustls::ClientConfig>,
    entries: impl IntoIterator<Item = (&'a str, &'a [Version])>,
    require_protocol: bool,
    path: Option<&str>,
) -> Result<Session, Error> {
    let stream = TcpStream::connect(&addr).await?;

    let server_name = rustls::pki_types::ServerName::try_from(server_name)
        .map_err(|_| Error::InvalidServerName)?
        .to_owned();

    let prefixed = alpn::build(entries, require_protocol);

    let mut config = (*config).clone();
    config.alpn_protocols = prefixed.iter().map(|s| s.as_bytes().to_vec()).collect();

    tracing::debug!(?prefixed, "TLS connecting");

    let connector = TlsConnector::from(Arc::new(config));
    let tls_stream = connector.connect(server_name, stream).await?;

    let negotiated = tls_stream.get_ref().1.alpn_protocol();
    let negotiated_str = negotiated.and_then(|a| std::str::from_utf8(a).ok());
    tracing::debug!(?negotiated_str, "TLS negotiated ALPN");

    let (version, protocol) = alpn::parse(negotiated_str);
    tracing::debug!(?version, ?protocol, "parsed ALPN");

    // In strict mode an unrecognized or absent ALPN would otherwise fall
    // through to the legacy `webtransport` wire format with no app protocol,
    // silently downgrading the negotiation we promised to enforce.
    if require_protocol && protocol.is_none() {
        return Err(Error::InvalidProtocol(
            negotiated_str.unwrap_or("<none>").to_string(),
        ));
    }

    let mut session_config = Config::negotiated(version, protocol);
    session_config.path = path.map(str::to_string);
    let transport = Stream::new(tls_stream, version, session_config.max_record_size);
    Ok(Session::connect(transport, session_config))
}

/// Accept a TLS connection.
///
/// Selection of an offered ALPN happens inside rustls based on the
/// `alpn_protocols` already set on the supplied `rustls::ServerConfig`; the
/// QMux wire-format version is then derived from the negotiated ALPN.
/// Callers building that ALPN list should use `{version.prefix()}{alpn}` for
/// each supported pair (e.g. `"qmux-01.moq-lite-04"`).
pub async fn accept(
    stream: TcpStream,
    config: Arc<rustls::ServerConfig>,
) -> Result<Session, Error> {
    let acceptor = TlsAcceptor::from(config);
    let tls_stream = acceptor.accept(stream).await?;

    let negotiated = tls_stream.get_ref().1.alpn_protocol();
    let negotiated_str = negotiated.and_then(|a| std::str::from_utf8(a).ok());
    tracing::debug!(?negotiated_str, "TLS accepted, negotiated ALPN");

    let (version, protocol) = alpn::parse(negotiated_str);
    tracing::debug!(?version, ?protocol, "parsed ALPN");

    let session_config = Config::negotiated(version, protocol);
    let transport = Stream::new(tls_stream, version, session_config.max_record_size);
    Ok(Session::accept(transport, session_config))
}
