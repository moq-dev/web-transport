//! ALPN / subprotocol negotiation helpers shared by TLS and WebSocket transports.

use crate::Version;

/// Build the ALPN list for a single QMux version and its application protocols.
///
/// Emits the bare version ALPN followed by `{prefix}{proto}` for each app protocol.
/// Callers that need to advertise multiple QMux versions (e.g. as fallback) should
/// build a list per version and try each in turn; this crate does not cross-product
/// versions on its own.
///
/// Returns strings suitable for TLS ALPN or WebSocket `Sec-WebSocket-Protocol`.
pub(crate) fn build(version: Version, app_protocols: &[String]) -> Vec<String> {
    let prefix = version.prefix();
    let mut alpns = Vec::with_capacity(1 + app_protocols.len());
    alpns.push(version.alpn().to_string());
    for proto in app_protocols {
        alpns.push(format!("{prefix}{proto}"));
    }
    alpns
}
