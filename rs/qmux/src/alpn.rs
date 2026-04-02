//! ALPN / subprotocol negotiation helpers shared by TLS and WebSocket transports.

use crate::Version;

/// Build the list of ALPN/subprotocol strings from application protocols.
///
/// For each version (in preference order) and each app protocol, offers
/// `{prefix}{proto}` plus the bare version ALPN as a fallback.
///
/// If `versions` is empty, all supported versions are offered.
///
/// Returns strings suitable for TLS ALPN or WebSocket `Sec-WebSocket-Protocol`.
pub(crate) fn build(app_protocols: &[String], versions: &[Version]) -> Vec<String> {
    let versions = if versions.is_empty() {
        Version::ALL
    } else {
        versions
    };

    let mut alpns = Vec::new();

    for &version in versions {
        let prefix = version.prefix();
        alpns.push(version.alpn().to_string());
        for proto in app_protocols {
            alpns.push(format!("{prefix}{proto}"));
        }
    }

    alpns
}
