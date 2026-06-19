//! In-band application-protocol negotiation over byte-stream transports
//! (the `application_protocols` QMux transport parameter).

#![cfg(feature = "tcp")]

use qmux::Version;
use tokio::net::TcpListener;
use web_transport_trait::Session as _;

/// Server preference wins, and both sides agree on the result.
#[tokio::test]
async fn tcp_negotiates_shared_protocol() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        // Server prefers moq-lite-03, but only moq-lite-04 is shared.
        qmux::tcp::accept_protocols(sock, Version::QMux01, &["moq-lite-03", "moq-lite-04"])
            .await
            .unwrap()
    });

    let client =
        qmux::tcp::connect_protocols(addr, Version::QMux01, &["moq-lite-04", "moq-lite-05"])
            .await
            .unwrap();

    let server = server.await.unwrap();

    assert_eq!(client.protocol(), Some("moq-lite-04"));
    assert_eq!(server.protocol(), Some("moq-lite-04"));
}

/// No shared protocol resolves to `None` on both sides (not an error).
#[tokio::test]
async fn tcp_no_overlap_resolves_to_none() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        qmux::tcp::accept_protocols(sock, Version::QMux01, &["moq-lite-99"])
            .await
            .unwrap()
    });

    let client = qmux::tcp::connect_protocols(addr, Version::QMux01, &["moq-lite-04"])
        .await
        .unwrap();
    let server = server.await.unwrap();

    assert_eq!(client.protocol(), None);
    assert_eq!(server.protocol(), None);
}

/// A peer that doesn't advertise protocols (plain `connect`) still interops;
/// the negotiating side just sees no agreed protocol.
#[tokio::test]
async fn tcp_legacy_peer_without_protocols() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        qmux::tcp::accept(sock, Version::QMux01).await.unwrap()
    });

    let client = qmux::tcp::connect_protocols(addr, Version::QMux01, &["moq-lite-04"])
        .await
        .unwrap();
    let server = server.await.unwrap();

    assert_eq!(client.protocol(), None);
    assert_eq!(server.protocol(), None);
}

#[cfg(unix)]
#[tokio::test]
async fn uds_negotiates_shared_protocol() {
    use tokio::net::UnixListener;

    let dir = std::env::temp_dir().join(format!("qmux-uds-{}", std::process::id()));
    let path = dir.with_extension("sock");
    // Best-effort cleanup of a stale socket from a previous crashed run.
    let _ = std::fs::remove_file(&path);

    let listener = UnixListener::bind(&path).unwrap();
    let accept_path = path.clone();

    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let session = qmux::uds::accept_protocols(sock, Version::QMux01, &["moq-lite-04"])
            .await
            .unwrap();
        let _ = accept_path;
        session
    });

    let client =
        qmux::uds::connect_protocols(&path, Version::QMux01, &["moq-lite-04", "moq-lite-03"])
            .await
            .unwrap();
    let server = server.await.unwrap();

    assert_eq!(client.protocol(), Some("moq-lite-04"));
    assert_eq!(server.protocol(), Some("moq-lite-04"));

    let _ = std::fs::remove_file(&path);
}
