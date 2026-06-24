//! In-band application-protocol negotiation over byte-stream transports
//! (the `application_protocols` QMux transport parameter).

#![cfg(any(feature = "tcp", feature = "uds"))]

use qmux::Version;
use web_transport_trait::Session as _;

#[cfg(feature = "tcp")]
mod tcp {
    use super::*;
    use qmux::Error;
    use tokio::net::TcpListener;

    /// Server preference wins, and both sides agree on the result.
    #[tokio::test]
    async fn negotiates_shared_protocol() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            // Server prefers moq-lite-03, but only moq-lite-04 is shared.
            qmux::tcp::Config::new(Version::QMux01)
                .protocols(["moq-lite-03", "moq-lite-04"])
                .accept(sock)
                .await
                .unwrap()
        });

        let client = qmux::tcp::Config::new(Version::QMux01)
            .protocols(["moq-lite-04", "moq-lite-05"])
            .connect(addr)
            .await
            .unwrap();
        let server = server.await.unwrap();

        assert_eq!(client.protocol(), Some("moq-lite-04"));
        assert_eq!(server.protocol(), Some("moq-lite-04"));
    }

    /// No shared protocol resolves to `None` on both sides (not an error).
    #[tokio::test]
    async fn no_overlap_resolves_to_none() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            qmux::tcp::Config::new(Version::QMux01)
                .protocols(["moq-lite-99"])
                .accept(sock)
                .await
                .unwrap()
        });

        let client = qmux::tcp::Config::new(Version::QMux01)
            .protocols(["moq-lite-04"])
            .connect(addr)
            .await
            .unwrap();
        let server = server.await.unwrap();

        assert_eq!(client.protocol(), None);
        assert_eq!(server.protocol(), None);
    }

    /// Neither side advertises protocols: no parameter on the wire, both `None`.
    #[tokio::test]
    async fn both_without_protocols() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            qmux::tcp::Config::new(Version::QMux01)
                .accept(sock)
                .await
                .unwrap()
        });

        let client = qmux::tcp::Config::new(Version::QMux01)
            .connect(addr)
            .await
            .unwrap();
        let server = server.await.unwrap();

        assert_eq!(client.protocol(), None);
        assert_eq!(server.protocol(), None);
    }

    /// Receiving the parameter while not negotiating is a fatal protocol error.
    /// Here the client advertises protocols but the server never opted in.
    #[tokio::test]
    async fn unexpected_protocols_is_fatal() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            // The client's unexpected `application_protocols` param fails the
            // handshake, so establishment (awaited inside `accept`) returns it.
            match qmux::tcp::Config::new(Version::QMux01).accept(sock).await {
                Err(e) => e,
                Ok(_) => panic!("expected establishment to fail"),
            }
        });

        // Keep the client alive so its parameters actually reach the server.
        let _client = qmux::tcp::Config::new(Version::QMux01)
            .protocols(["moq-lite-04"])
            .connect(addr)
            .await
            .unwrap();
        let err = server.await.unwrap();

        assert!(matches!(err, Error::UnexpectedProtocols), "got {err:?}");
    }

    /// A peer that completes the TCP handshake but never sends its transport
    /// parameters fails establishment with `HandshakeTimeout` instead of hanging.
    #[tokio::test]
    async fn handshake_timeout_fires() {
        use std::time::Duration;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Accept the connection but never speak QMux, holding the socket open so
        // the client can't short-circuit on an EOF — it must hit the timeout.
        let server = tokio::spawn(async move {
            let (_sock, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        });

        let result = qmux::tcp::Config::new(Version::QMux01)
            .handshake_timeout(Duration::from_millis(100))
            .connect(addr)
            .await;

        match result {
            Err(Error::HandshakeTimeout) => {}
            Err(other) => panic!("expected HandshakeTimeout, got {other:?}"),
            Ok(_) => panic!("expected HandshakeTimeout, got an established session"),
        }
        server.abort();
    }

    /// Advertised protocol names are validated before the session starts.
    #[tokio::test]
    async fn rejects_invalid_protocol_name() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // A space isn't a valid protocol token, so this fails during connect
        // (after the TCP handshake, before any QMux frames are exchanged).
        // `Session` isn't `Debug`, so match rather than `unwrap_err`.
        match qmux::tcp::Config::new(Version::QMux01)
            .protocols(["bad name"])
            .connect(addr)
            .await
        {
            Err(Error::InvalidProtocol(_)) => {}
            Err(other) => panic!("wrong error: {other:?}"),
            Ok(_) => panic!("expected InvalidProtocol error"),
        }
    }
}

#[cfg(all(unix, feature = "uds"))]
mod uds {
    use super::*;
    use tokio::net::UnixListener;

    #[tokio::test]
    async fn negotiates_shared_protocol() {
        let path = std::env::temp_dir().join(format!("qmux-uds-{}.sock", std::process::id()));
        // Best-effort cleanup of a stale socket from a previous crashed run.
        let _ = std::fs::remove_file(&path);

        let listener = UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            qmux::uds::Config::new(Version::QMux01)
                .protocols(["moq-lite-04"])
                .accept(sock)
                .await
                .unwrap()
        });

        let client = qmux::uds::Config::new(Version::QMux01)
            .protocols(["moq-lite-04", "moq-lite-03"])
            .connect(&path)
            .await
            .unwrap();
        let server = server.await.unwrap();

        assert_eq!(client.protocol(), Some("moq-lite-04"));
        assert_eq!(server.protocol(), Some("moq-lite-04"));

        let _ = std::fs::remove_file(&path);
    }
}
