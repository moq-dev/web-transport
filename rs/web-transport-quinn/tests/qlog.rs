//! The `qlog` feature must keep `quinn`'s qlog knobs reachable through our re-export.

#![cfg(feature = "qlog")]

use web_transport_quinn::quinn;

#[test]
fn transport_config_exposes_qlog() {
    let mut config = quinn::QlogConfig::default();
    config.writer(Box::new(std::io::sink()));

    let mut transport = quinn::TransportConfig::default();
    transport.qlog_stream(config.into_stream());
}
