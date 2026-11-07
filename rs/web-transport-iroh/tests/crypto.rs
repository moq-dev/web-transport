use std::sync::Arc;

use rustls::crypto::CryptoProvider;
use web_transport_iroh::crypto;

// This integration test has its own process so installing a provider cannot
// change the provider used by the transport tests.
#[test]
fn provider_selection() {
    assert!(CryptoProvider::get_default().is_none());
    let fallback = crypto::default_provider().unwrap();
    assert!(CryptoProvider::get_default().is_none());
    assert_eq!(
        fallback.cipher_suites,
        rind_rustls::default_provider().unwrap().cipher_suites
    );

    let mut installed = (*fallback).clone();
    installed.cipher_suites = vec![rind_rustls::cipher_suite::TLS13_CHACHA20_POLY1305_SHA256];
    installed.install_default().unwrap();
    assert!(Arc::ptr_eq(
        &crypto::default_provider().unwrap(),
        CryptoProvider::get_default().unwrap()
    ));
}
