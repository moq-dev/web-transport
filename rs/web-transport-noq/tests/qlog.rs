//! The `qlog` feature must keep `noq`'s qlog knobs reachable through our re-export.

#![cfg(feature = "qlog")]

use web_transport_noq::noq;

/// `TransportConfig`'s Debug reports whether a qlog factory is installed, which is the
/// only observable state noq exposes for it.
fn qlog_enabled(transport: &noq::TransportConfig) -> bool {
    format!("{transport:?}").contains("qlog_factory: true")
}

#[test]
fn transport_config_exposes_qlog() {
    let mut transport = noq::TransportConfig::default();
    assert!(!qlog_enabled(&transport), "qlog should start disabled");

    transport.qlog_from_path(std::env::temp_dir(), "web-transport-noq");

    assert!(
        qlog_enabled(&transport),
        "qlog_from_path did not install a factory"
    );
}
