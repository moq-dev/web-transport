//! Receive streams release connection credit even when discarded unread.
#![cfg(feature = "tcp")]

use qmux::{transport::Stream, Config, Session, Version};
use std::time::Duration;
use web_transport_trait::{SendStream as _, Session as _};

#[tokio::test(start_paused = true)]
async fn unread_streams_deliver_beyond_the_connection_window() {
    for version in [Version::QMux00, Version::QMux01, Version::QMux02] {
        let mut config = Config::new(version);
        config.max_data = 4;
        config.max_streams_uni = 1;
        config.max_stream_data_uni = 4;
        let (client_io, server_io) = tokio::io::duplex(1024);
        let (client, server) = tokio::join!(
            Session::connect(
                Stream::new(client_io, version, config.max_record_size),
                config.clone()
            ),
            Session::accept(
                Stream::new(server_io, version, config.max_record_size),
                config
            ),
        );
        let (client, server) = (client.unwrap(), server.unwrap());
        tokio::time::timeout(Duration::from_secs(2), async {
            for _ in 0..16 {
                let mut send = client.open_uni().await.unwrap();
                send.write(b"data").await.unwrap();
                send.finish().unwrap();
                drop(server.accept_uni().await.unwrap());
            }
        })
        .await
        .expect("unread streams exhausted MAX_DATA");
    }
}

#[tokio::test(start_paused = true)]
async fn byte_stream_peer_observes_close_reason_after_last_handle_drops() {
    for version in [Version::QMux00, Version::QMux01, Version::QMux02] {
        let config = Config::new(version);
        let (client_io, server_io) = tokio::io::duplex(1024);
        let (client, server) = tokio::join!(
            Session::connect(
                Stream::new(client_io, version, config.max_record_size),
                config.clone()
            ),
            Session::accept(
                Stream::new(server_io, version, config.max_record_size),
                config
            ),
        );
        let (client, server) = (client.unwrap(), server.unwrap());
        let mut send = client.open_uni().await.unwrap();
        send.write(b"data").await.unwrap();
        client.close(42, "bye");
        drop(client);
        let reason = tokio::time::timeout(Duration::from_secs(2), server.closed())
            .await
            .unwrap();
        assert!(
            matches!(reason, qmux::Error::ConnectionClosed { code, .. } if code.into_inner() == 42)
        );
    }
}
