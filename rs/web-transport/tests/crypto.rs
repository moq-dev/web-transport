#![cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "aws-lc-rs", feature = "ring")
))]

#[test]
fn builder_provider_selection() {
    #[cfg(all(feature = "aws-lc-rs", feature = "ring"))]
    {
        use web_transport::quinn::quinn::rustls::crypto;

        assert!(crypto::CryptoProvider::get_default().is_none());
        assert!(std::panic::catch_unwind(web_transport::ClientBuilder::new).is_err());

        crypto::ring::default_provider().install_default().unwrap();
    }

    let _ = web_transport::ClientBuilder::new();
}
