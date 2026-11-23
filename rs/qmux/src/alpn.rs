//! ALPN / subprotocol negotiation helpers shared by TLS and WebSocket transports.
//!
//! Each entry is `(alpn, versions)`: a single ALPN paired with the QMux
//! wire-format versions it can ride on. The wire form `{version.prefix()}{alpn}`
//! is emitted once per version, in order. An empty `versions` slice means
//! "every QMux draft this crate knows about" (see [`QMUX_VERSIONS`]).
//!
//! Bare version ALPNs (`qmux-02`, `qmux-01`, `qmux-00`, `webtransport`, no app
//! protocol attached) are advertised/accepted by default, so a peer that only
//! knows a wire-format version can still connect. Set the `require_protocol`
//! flag to suppress them and advertise/accept only the configured prefixed pairs.
//!
//! [`parse`] recovers `(Version, Option<String>)` from a negotiated wire-format
//! ALPN. Only the QMux drafts appear in `{prefix}{alpn}` form; the legacy
//! `webtransport` wire format only shows up as the bare ALPN, so this module
//! never emits or strips a `webtransport.` prefix.

use crate::{Error, Version};

/// QMux versions that can ride under a `{prefix}{alpn}` pair, newest first.
pub(crate) const QMUX_VERSIONS: &[Version] = &[Version::QMux02, Version::QMux01, Version::QMux00];

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
/// Unless `require_protocol` is set, the bare version ALPNs (`qmux-02`,
/// `qmux-01`, `qmux-00`, `webtransport`) are appended after the prefixed pairs
/// as a fallback for peers that don't want to commit to an app protocol.
///
/// Suitable for TLS ALPN or WebSocket `Sec-WebSocket-Protocol`.
///
/// Fails with [`Error::InvalidProtocol`] when an entry lists
/// `Version::WebTransport`: that format has no prefixed form, and its bare
/// ALPN is controlled by `require_protocol`.
pub(crate) fn build<'a>(
    entries: impl IntoIterator<Item = (&'a str, &'a [Version])>,
    require_protocol: bool,
) -> Result<Vec<String>, Error> {
    let mut out: Vec<String> = Vec::new();
    for (alpn, versions) in entries {
        for &version in expand_versions(versions) {
            let wire = format!("{}{}", version.prefix(), alpn);
            if !version.is_qmux() {
                return Err(Error::InvalidProtocol(wire));
            }
            out.push(wire);
        }
    }
    if !require_protocol {
        // Every bare version ALPN, newest first.
        for &v in Version::ALL {
            out.push(v.alpn().to_string());
        }
    }
    Ok(out)
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
        let out = build([("moq-lite-04", &[Version::QMux01][..])], false).unwrap();
        assert_eq!(
            out,
            vec![
                "qmux-01.moq-lite-04",
                "qmux-02",
                "qmux-01",
                "qmux-00",
                "webtransport"
            ]
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
        )
        .unwrap();
        assert_eq!(out, vec!["qmux-01.moq-lite-04", "qmux-00.moq-transport-17"]);
    }

    #[test]
    fn build_expands_empty_versions_to_all_qmux_drafts() {
        let out = build([("moq-lite-04", &[][..])], true).unwrap();
        assert_eq!(
            out,
            vec![
                "qmux-02.moq-lite-04",
                "qmux-01.moq-lite-04",
                "qmux-00.moq-lite-04"
            ]
        );
    }

    #[test]
    fn build_emits_one_pair_per_listed_version() {
        let out = build(
            [("moq-lite-04", &[Version::QMux00, Version::QMux01][..])],
            true,
        )
        .unwrap();
        assert_eq!(out, vec!["qmux-00.moq-lite-04", "qmux-01.moq-lite-04"]);
    }

    #[test]
    fn build_empty_by_default_emits_only_bare_alpns() {
        let entries: [(&str, &[Version]); 0] = [];
        let out = build(entries, false).unwrap();
        assert_eq!(out, vec!["qmux-02", "qmux-01", "qmux-00", "webtransport"]);
    }

    #[test]
    fn build_empty_with_require_protocol_emits_nothing() {
        let entries: [(&str, &[Version]); 0] = [];
        let out = build(entries, true).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn build_rejects_webtransport_in_an_entry() {
        let err = build(
            [("test", &[Version::QMux01, Version::WebTransport][..])],
            false,
        )
        .unwrap_err();
        assert!(matches!(err, Error::InvalidProtocol(wire) if wire == "webtransport.test"));
    }

    #[test]
    fn parse_recognises_prefixed_pairs() {
        assert_eq!(
            parse(Some("qmux-02.moq-lite-04")),
            (Version::QMux02, Some("moq-lite-04".to_string()))
        );
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
        assert_eq!(parse(Some("qmux-02")), (Version::QMux02, None));
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
