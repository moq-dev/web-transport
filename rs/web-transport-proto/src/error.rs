// WebTransport shares with HTTP/3, so we can't start at 0 or use the full VarInt.
const ERROR_FIRST: u64 = 0x52e4a40fa8db;
const ERROR_LAST: u64 = 0x52e5ac983162;

/// Decode a WebTransport application error code from an HTTP/3 error code, or
/// `None` if the code is outside the WebTransport range or a reserved codepoint.
pub const fn error_from_http3(code: u64) -> Option<u32> {
    // Reserved (GREASE) codepoints sit inside the range but encode no application code.
    if code < ERROR_FIRST || code > ERROR_LAST || (code - 0x21).is_multiple_of(0x1f) {
        return None;
    }

    let code = code - ERROR_FIRST;
    let code = code - code / 0x1f;

    Some(code as u32)
}

/// Encode a WebTransport application error code as an HTTP/3 error code,
/// skipping the reserved codepoints.
pub const fn error_to_http3(code: u32) -> u64 {
    ERROR_FIRST + code as u64 + code as u64 / 0x1e
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_reserved(code: u64) -> bool {
        code >= 0x21 && (code - 0x21).is_multiple_of(0x1f)
    }

    #[test]
    fn range_bounds() {
        assert_eq!(error_to_http3(0), ERROR_FIRST);
        assert_eq!(error_to_http3(u32::MAX), ERROR_LAST);
        assert_eq!(error_from_http3(ERROR_FIRST), Some(0));
        assert_eq!(error_from_http3(ERROR_LAST), Some(u32::MAX));
        assert_eq!(error_from_http3(ERROR_FIRST - 1), None);
        assert_eq!(error_from_http3(ERROR_LAST + 1), None);
    }

    /// Every HTTP/3 code near the range edges either decodes to the application
    /// code that encodes back to it, or is reserved and decodes to nothing.
    #[test]
    fn decode_inverts_encode() {
        let edges =
            (ERROR_FIRST - 64..ERROR_FIRST + 4096).chain(ERROR_LAST - 4096..ERROR_LAST + 64);
        for http3 in edges {
            match error_from_http3(http3) {
                Some(code) => assert_eq!(error_to_http3(code), http3, "{http3:#x}"),
                None => assert!(
                    !(ERROR_FIRST..=ERROR_LAST).contains(&http3) || is_reserved(http3),
                    "{http3:#x} has no application code"
                ),
            }
        }
    }

    #[test]
    fn encode_skips_reserved() {
        let codes = (0..4096).chain(u32::MAX - 4096..=u32::MAX);
        for code in codes {
            let http3 = error_to_http3(code);
            assert!(!is_reserved(http3), "{code} encodes to reserved {http3:#x}");
            assert_eq!(error_from_http3(http3), Some(code));
        }
    }
}
