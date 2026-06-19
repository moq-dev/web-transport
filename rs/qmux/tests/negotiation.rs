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
            let session = qmux::tcp::Config::new(Version::QMux01)
                .accept(sock)
                .await
                .unwrap();
            session.closed().await
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
