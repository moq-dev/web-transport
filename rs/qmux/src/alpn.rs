//! ALPN / subprotocol negotiation helpers shared by TLS and WebSocket transports.
//!
//! Each entry is `(alpn, versions)`: a single ALPN paired with the QMux
//! wire-format versions it can ride on. The wire form `{version.prefix()}{alpn}`
//! is emitted once per version, in order. An empty `versions` slice means
//! "every QMux draft this crate knows about" (see [`QMUX_VERSIONS`]).
//!
//! Bare version ALPNs (`qmux-01`, `qmux-00`, `webtransport`, no app protocol
//! attached) are advertised/accepted by default, so a peer that only knows a
//! wire-format version can still connect. Set the `require_protocol` flag to
//! suppress them and advertise/accept only the configured prefixed pairs.
//!
//! [`parse`] recovers `(Version, Option<String>)` from a negotiated wire-format
//! ALPN. Only the QMux drafts appear in `{prefix}{alpn}` form; the legacy
//! `webtransport` wire format only shows up as the bare ALPN, so this module
//! never emits or strips a `webtransport.` prefix.

use crate::Version;

/// QMux versions that can ride under a `{prefix}{alpn}` pair, newest first.
pub(crate) const QMUX_VERSIONS: &[Version] = &[Version::QMux01, Version::QMux00];

/// Bare version ALPNs added (offered by clients, accepted by servers) unless
/// the caller opts out via `require_protocol`. Newest first.
pub(crate) const BARE_ALPNS: &[Version] =
    &[Version::QMux01, Version::QMux00, Version::WebTransport];

/// Resolve an entry's `versions` slice: empty falls back to every supported
/// QMux draft, mirroring JS's `null` value.
pub(crate) fn expand_versions(versions: &[Version]) -> &[Version] {
    if versions.is_empty() {
        QMUX_VERSIONS
    } else {
        versions
    }
}

/// Build the ALPN list from `(alpn, versions)` entries.
///
/// Each entry emits `{v.prefix()}{alpn}` per version in `expand_versions(versions)`.
/// Unless `require_protocol` is set, the bare version ALPNs (`qmux-01`,
/// `qmux-00`, `webtransport`) are appended after the prefixed pairs as a
/// fallback for peers that don't want to commit to an app protocol.
///
/// Suitable for TLS ALPN or WebSocket `Sec-WebSocket-Protocol`.
///
/// Passing `Version::WebTransport` inside an entry's `versions` is a usage
/// bug (the bare ALPN is controlled by `require_protocol`, not by listing it
/// as a pair version) and panics in debug builds.
pub(crate) fn build<'a>(
    entries: impl IntoIterator<Item = (&'a str, &'a [Version])>,
    require_protocol: bool,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (alpn, versions) in entries {
        for &version in expand_versions(versions) {
            debug_assert!(
                version.is_qmux(),
                "webtransport doesn't use prefixed ALPN; pair entries are qmux only. Bare ALPNs are controlled by require_protocol."
            );
            out.push(format!("{}{}", version.prefix(), alpn));
        }
    }
    if !require_protocol {
        for &v in BARE_ALPNS {
            out.push(v.alpn().to_string());
        }
    }
    out
}

/// Parse a negotiated wire-format ALPN into `(version, app_protocol)`.
///
/// Recognises:
///   - `{qmux-VV.}{alpn}` -> `(QMuxVV, Some(alpn))`
///   - the bare version ALPN `qmux-VV` or `webtransport` -> `(matching, None)`
///
/// Empty or unrecognised values fall back to `(WebTransport, None)`, which the
/// caller treats as the pre-QMux wire format.
pub(crate) fn parse(alpn: Option<&str>) -> (Version, Option<String>) {
    let alpn = match alpn {
        Some(s) if !s.is_empty() => s,
        _ => return (Version::WebTransport, None),
    };

    for &v in QMUX_VERSIONS {
        if alpn == v.alpn() {
            return (v, None);
        }
        if let Some(rest) = alpn.strip_prefix(v.prefix()) {
            let app = (!rest.is_empty()).then(|| rest.to_string());
            return (v, app);
        }
    }

    // Bare "webtransport" lands here too; treat anything that's not a QMux ALPN
    // as the legacy wire format.
    (Version::WebTransport, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_appends_bare_alpns_by_default() {
        let out = build([("moq-lite-04", &[Version::QMux01][..])], false);
        assert_eq!(
            out,
            vec!["qmux-01.moq-lite-04", "qmux-01", "qmux-00", "webtransport"]
        );
    }

    #[test]
    fn build_emits_only_prefixed_pairs_when_require_protocol() {
        let out = build(
            [
                ("moq-lite-04", &[Version::QMux01][..]),
                ("moq-transport-17", &[Version::QMux00][..]),
            ],
            true,
        );
        assert_eq!(out, vec!["qmux-01.moq-lite-04", "qmux-00.moq-transport-17"]);
    }

    #[test]
    fn build_expands_empty_versions_to_all_qmux_drafts() {
        let out = build([("moq-lite-04", &[][..])], true);
        assert_eq!(out, vec!["qmux-01.moq-lite-04", "qmux-00.moq-lite-04"]);
    }

    #[test]
    fn build_emits_one_pair_per_listed_version() {
        let out = build(
            [("moq-lite-04", &[Version::QMux00, Version::QMux01][..])],
            true,
        );
        assert_eq!(out, vec!["qmux-00.moq-lite-04", "qmux-01.moq-lite-04"]);
    }

    #[test]
    fn build_empty_by_default_emits_only_bare_alpns() {
        let entries: [(&str, &[Version]); 0] = [];
        let out = build(entries, false);
        assert_eq!(out, vec!["qmux-01", "qmux-00", "webtransport"]);
    }

    #[test]
    fn build_empty_with_require_protocol_emits_nothing() {
        let entries: [(&str, &[Version]); 0] = [];
        let out = build(entries, true);
        assert!(out.is_empty());
    }

    #[test]
    fn parse_recognises_prefixed_pairs() {
        assert_eq!(
            parse(Some("qmux-01.moq-lite-04")),
            (Version::QMux01, Some("moq-lite-04".to_string()))
        );
        assert_eq!(
            parse(Some("qmux-00.moq-transport-17")),
            (Version::QMux00, Some("moq-transport-17".to_string()))
        );
    }

    #[test]
    fn parse_recognises_bare_versions() {
        assert_eq!(parse(Some("qmux-01")), (Version::QMux01, None));
        assert_eq!(parse(Some("qmux-00")), (Version::QMux00, None));
        assert_eq!(parse(Some("webtransport")), (Version::WebTransport, None));
    }

    #[test]
    fn parse_falls_back_to_webtransport() {
        assert_eq!(parse(None), (Version::WebTransport, None));
        assert_eq!(parse(Some("")), (Version::WebTransport, None));
        assert_eq!(parse(Some("h2")), (Version::WebTransport, None));
    }

    #[test]
    fn parse_does_not_strip_webtransport_prefix() {
        // We don't recognise `webtransport.X`; treat it as legacy bare.
        assert_eq!(
            parse(Some("webtransport.moq-lite-04")),
            (Version::WebTransport, None)
        );
    }
}
