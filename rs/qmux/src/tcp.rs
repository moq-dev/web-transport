use tokio::net::{TcpStream, ToSocketAddrs};

use crate::transport::{build_stream_session, protocol_config};
use crate::{Config, Error, Session, Version};

/// Connect to a TCP server and speak QMux at `version`.
///
/// TCP has no protocol negotiation, so the version is a direct parameter
/// instead of derived from an ALPN. Use [`connect_protocols`] to also negotiate
/// an application protocol in-band.
pub async fn connect(addr: impl ToSocketAddrs, version: Version) -> Result<Session, Error> {
    let stream = TcpStream::connect(addr).await?;
    Ok(build_stream_session(
        stream,
        Config::new(version, None),
        false,
    ))
}

/// Accept a TCP connection and speak QMux at `version`.
pub async fn accept(stream: TcpStream, version: Version) -> Result<Session, Error> {
    Ok(build_stream_session(
        stream,
        Config::new(version, None),
        true,
    ))
}

/// Connect and negotiate an application protocol in-band.
///
/// TCP has no ALPN, so the `protocols` (in preference order) are advertised via
/// the `application_protocols` QMux transport parameter. This awaits the peer's
/// parameters and resolves the agreed protocol — readable afterwards from
/// [`Session::protocol`](web_transport_trait::Session::protocol) — before
/// returning. The agreed protocol is the first one both sides offered, with the
/// server's preference order winning; it is `None` if they don't overlap.
pub async fn connect_protocols(
    addr: impl ToSocketAddrs,
    version: Version,
    protocols: &[&str],
) -> Result<Session, Error> {
    let stream = TcpStream::connect(addr).await?;
    let session = build_stream_session(stream, protocol_config(version, protocols)?, false);
    session.negotiated().await;
    Ok(session)
}

/// Accept a TCP connection and negotiate an application protocol in-band.
///
/// The server-side counterpart to [`connect_protocols`]; `protocols` is this
/// peer's supported set in preference order (which wins ties). Awaits the
/// client's parameters before returning.
pub async fn accept_protocols(
    stream: TcpStream,
    version: Version,
    protocols: &[&str],
) -> Result<Session, Error> {
    let session = build_stream_session(stream, protocol_config(version, protocols)?, true);
    session.negotiated().await;
    Ok(session)
}
