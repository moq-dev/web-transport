/// Wrap user-provided JavaScript test code with a `connectWebTransport()` helper
/// and error handling.
///
/// The returned string is a self-contained async IIFE that can be evaluated
/// directly via `page.evaluate()`.
pub fn wrap_test_js(server_url: &str, certificate_hash: &[u8], user_code: &str) -> String {
    let hash_bytes = certificate_hash
        .iter()
        .map(|b| b.to_string())
        .collect::<Vec<_>>()
        .join(", ");

    format!(
        r#"(async () => {{
    const SERVER_URL = "{server_url}";
    const CERT_HASH = new Uint8Array([{hash_bytes}]);

    async function connectWebTransport(url, options) {{
        const wt = new WebTransport(url || SERVER_URL, {{
            serverCertificateHashes: [{{
                algorithm: "sha-256",
                value: CERT_HASH,
            }}],
            ...(options || {{}}),
        }});
        await wt.ready;
        return wt;
    }}

    try {{
        const result = await (async () => {{ {user_code} }})();
        if (result === undefined || result === null) {{
            return JSON.stringify({{ success: false, message: "test code did not return a result" }});
        }}
        return JSON.stringify(result);
    }} catch (e) {{
        const msg = "JS exception: " + (e.stack ? e.stack : e.toString());
        return JSON.stringify({{
            success: false,
            message: msg,
            details: {{ name: e.name, stack: e.stack }}
        }});
    }}
}})()"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_produces_valid_js() {
        let js = wrap_test_js(
            "https://localhost:4443",
            &[0xab, 0xcd, 0x01],
            "return { success: true, message: 'hello' };",
        );
        assert!(js.contains("SERVER_URL"));
        assert!(js.contains("171, 205, 1"));
        assert!(js.contains("connectWebTransport"));
        assert!(js.contains("return { success: true, message: 'hello' };"));
    }
}
