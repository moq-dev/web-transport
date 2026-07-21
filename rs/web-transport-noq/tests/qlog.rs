//! The `qlog` feature must keep `noq`'s qlog knobs reachable through our re-export.

#![cfg(feature = "qlog")]

use web_transport_noq::noq;

#[test]
fn transport_config_exposes_qlog() {
    let mut transport = noq::TransportConfig::default();
    transport.qlog_from_path(std::env::temp_dir(), "web-transport-noq");
}
