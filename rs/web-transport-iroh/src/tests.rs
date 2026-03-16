use std::time::Duration;

use iroh::{Endpoint, endpoint::ConnectionError};
use n0_tracing_test::traced_test;
use tokio::time::timeout;
use tracing::Instrument;
use url::Url;

use crate::{ALPN_H3, Client, H3Request, QuicRequest, SessionError};

#[tokio::test]
#[traced_test]
async fn h3_smoke() -> n0_error::Result<()> {
    let client = Endpoint::empty_builder()
        .bind()
        .instrument(tracing::error_span!("client-ep"))
        .await
        .unwrap();
    let client_id = client.id();
    let client = Client::new(client);

    let server = Endpoint::empty_builder()
        .alpns(vec![ALPN_H3.as_bytes().to_vec()])
        .bind()
        .instrument(tracing::error_span!("server-ep"))
        .await
        .unwrap();
    let server_id = server.id();
    let server_addr = server.addr();

    let url: Url = format!("https://{}/foo", server_id).parse().unwrap();

    let client_task = tokio::task::spawn({
        let url = url.clone();
        async move {
            let session = client.connect_h3(server_addr, url.clone()).await.inspect_err(|err| println!("{err:#?}")).unwrap();
            assert_eq!(session.remote_id(), server_id);
            assert_eq!(session.request().map(|r| &r.url), Some(&url));

            let mut stream = session.open_uni().await.unwrap();
            stream.write_all(b"hi").await.unwrap();
            stream.finish().unwrap();
            let reason = session.closed().await;
            assert!(
                matches!(reason, SessionError::ConnectionError(ConnectionError::ApplicationClosed(frame)) if web_transport_proto::error_from_http3(frame.error_code.into_inner()) == Some(23))
            );

            drop(session);
            client.close().await;
        }.instrument(tracing::error_span!("client"))
    });

    let server_task = tokio::task::spawn(
        async move {
            let conn = server.accept().await.unwrap().await.unwrap();
            assert_eq!(conn.alpn(), ALPN_H3.as_bytes());
            let request = H3Request::accept(conn)
                .await
                .inspect_err(|err| tracing::error!("accept failed: {err:?}"))
                .unwrap();
            assert_eq!(request.url, url);
            assert_eq!(request.conn().remote_id(), client_id);
            let session = request.ok().await.unwrap();
            assert_eq!(session.request().map(|r| &r.url), Some(&url));
            assert_eq!(session.conn().remote_id(), client_id);
            let mut stream = session.accept_uni().await.unwrap();
            let buf = stream.read_to_end(2).await.unwrap();
            assert_eq!(buf, b"hi");
            session.close(23, b"bye");
            server.close().await;
        }
        .instrument(tracing::error_span!("server")),
    );

    timeout(Duration::from_secs(10), client_task)
        .await
        .expect("client task timed out")
        .unwrap();
    timeout(Duration::from_secs(10), server_task)
        .await
        .expect("server task timed out")
        .unwrap();

    Ok(())
}

#[tokio::test]
#[traced_test]
async fn quic_smoke() -> n0_error::Result<()> {
    const ALPN: &str = "moql";

    let client = Endpoint::empty_builder().bind().await.unwrap();
    let client_id = client.id();
    let client = Client::new(client);

    let server = Endpoint::empty_builder()
        .alpns(vec![ALPN.as_bytes().to_vec()])
        .bind()
        .await
        .unwrap();
    let server_id = server.id();
    let server_addr = server.addr();

    let client_task = tokio::task::spawn({
        async move {
            let session = client
                .connect_quic(server_addr, ALPN.as_bytes())
                .await
                .unwrap();
            println!("session established");
            assert_eq!(session.remote_id(), server_id);
            assert!(session.request().is_none());
            let reason = session.closed().await;
            assert!(
                matches!(reason, SessionError::ConnectionError(ConnectionError::ApplicationClosed(frame)) if frame.error_code.into_inner() == 23)
            );
            client.close().await;
        }.instrument(tracing::error_span!("client"))
    });

    let server_task = tokio::task::spawn({
        async move {
            let conn = server.accept().await.unwrap().await.unwrap();
            assert_eq!(conn.alpn(), ALPN.as_bytes());
            let request = QuicRequest::accept(conn);
            assert_eq!(request.conn().remote_id(), client_id);
            let session = request.ok();
            assert!(session.request().is_none());
            assert_eq!(session.conn().remote_id(), client_id);
            session.close(23, b"bye");
            server.close().await;
        }
        .instrument(tracing::error_span!("server"))
    });

    timeout(Duration::from_secs(10), client_task)
        .await
        .expect("client task timed out")
        .unwrap();
    timeout(Duration::from_secs(10), server_task)
        .await
        .expect("server task timed out")
        .unwrap();

    Ok(())
}
