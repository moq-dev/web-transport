#![cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "aws-lc-rs", feature = "ring")
))]

#[test]
fn builder_provider_selection() {
    use web_transport::quinn::quinn::rustls::crypto;

    #[cfg(feature = "ring")]
    let expected = crypto::ring::default_provider();
    #[cfg(not(feature = "ring"))]
    let expected = crypto::aws_lc_rs::default_provider();

    #[cfg(all(feature = "aws-lc-rs", feature = "ring"))]
    {
        assert!(crypto::CryptoProvider::get_default().is_none());
        assert!(std::panic::catch_unwind(web_transport::ClientBuilder::new).is_err());

        expected.clone().install_default().unwrap();
    }

    let _ = web_transport::ClientBuilder::new();
    let actual = web_transport::quinn::crypto::default_provider();
    assert_eq!(actual.cipher_suites, expected.cipher_suites);
    assert_eq!(
        actual
            .kx_groups
            .iter()
            .map(|g| g.name())
            .collect::<Vec<_>>(),
        expected
            .kx_groups
            .iter()
            .map(|g| g.name())
            .collect::<Vec<_>>()
    );

    #[cfg(all(feature = "aws-lc-rs", feature = "ring"))]
    assert!(std::sync::Arc::ptr_eq(
        &actual,
        crypto::CryptoProvider::get_default().unwrap()
    ));
}
