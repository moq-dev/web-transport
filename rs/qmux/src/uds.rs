use std::path::Path;

use tokio::net::UnixStream;

use crate::transport::{build_stream_session, protocol_config};
use crate::{Config, Error, Session, Version};

/// Connect to a Unix domain socket and speak QMux at `version`.
///
/// Unix sockets have no protocol negotiation, so the version is a direct
/// parameter instead of derived from an ALPN. Use [`connect_protocols`] to also
/// negotiate an application protocol in-band. Ideal for same-host MoQ where the
/// TLS/ALPN handshake of a network transport would be pure overhead.
pub async fn connect(path: impl AsRef<Path>, version: Version) -> Result<Session, Error> {
    let stream = UnixStream::connect(path).await?;
    Ok(build_stream_session(
        stream,
        Config::new(version, None),
        false,
    ))
}

/// Accept a Unix domain socket connection and speak QMux at `version`.
pub async fn accept(stream: UnixStream, version: Version) -> Result<Session, Error> {
    Ok(build_stream_session(
        stream,
        Config::new(version, None),
        true,
    ))
}

/// Connect over a Unix domain socket and negotiate an application protocol
/// in-band.
///
/// Like [`crate::tcp::connect_protocols`], but over a Unix socket: `protocols`
/// (preference order) are advertised via the `application_protocols` QMux
/// transport parameter. Awaits the peer's parameters and resolves the agreed
/// protocol — readable from
/// [`Session::protocol`](web_transport_trait::Session::protocol) — before
/// returning.
pub async fn connect_protocols(
    path: impl AsRef<Path>,
    version: Version,
    protocols: &[&str],
) -> Result<Session, Error> {
    let stream = UnixStream::connect(path).await?;
    let session = build_stream_session(stream, protocol_config(version, protocols)?, false);
    session.negotiated().await;
    Ok(session)
}

/// Accept a Unix domain socket connection and negotiate an application protocol
/// in-band. The server-side counterpart to [`connect_protocols`].
pub async fn accept_protocols(
    stream: UnixStream,
    version: Version,
    protocols: &[&str],
) -> Result<Session, Error> {
    let session = build_stream_session(stream, protocol_config(version, protocols)?, true);
    session.negotiated().await;
    Ok(session)
}
