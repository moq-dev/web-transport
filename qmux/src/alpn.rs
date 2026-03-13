//! ALPN / subprotocol negotiation helpers shared by TLS and WebSocket transports.

use crate::{PREFIX_QMUX, PREFIX_WEBTRANSPORT};

/// Build the list of ALPN/subprotocol strings from application protocols.
///
/// For each app protocol, offers both `qmux-00.{proto}` and `webtransport.{proto}`.
/// Also includes bare `qmux-00` and `webtransport` as fallbacks.
///
/// Returns strings suitable for TLS ALPN or WebSocket `Sec-WebSocket-Protocol`.
pub(crate) fn build(app_protocols: &[String]) -> Vec<String> {
    let mut alpns = Vec::new();

    alpns.push(crate::ALPN_QMUX.to_string());
    for proto in app_protocols {
        alpns.push(format!("{PREFIX_QMUX}{proto}"));
    }
    alpns.push(crate::ALPN_WEBTRANSPORT.to_string());
    for proto in app_protocols {
        alpns.push(format!("{PREFIX_WEBTRANSPORT}{proto}"));
    }

    alpns
}
