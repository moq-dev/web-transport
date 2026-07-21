//! `Settings` must keep tokio-quiche's qlog knobs reachable, compression included.

use web_transport_quiche::{QlogCompression, Settings};

#[test]
fn settings_expose_qlog() {
    let mut settings = Settings::default();
    settings.qlog_dir = Some("/tmp/qlog".to_string());
    settings.qlog_compression = QlogCompression::Gzip;

    assert_eq!(settings.qlog_dir.as_deref(), Some("/tmp/qlog"));
    assert_eq!(settings.qlog_compression, QlogCompression::Gzip);
}
