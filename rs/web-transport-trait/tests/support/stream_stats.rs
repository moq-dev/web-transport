//! Shared conformance checks, included by each native backend's integration tests.

use bytes::{Buf, Bytes, BytesMut};
use web_transport_trait::{RecvStream, SendStream, Session};

pub async fn payload_stats<S: Session>(client: &S, server: &S) {
    let mut send = client.open_uni().await.unwrap();
    assert_eq!(send.stats().await.unwrap().bytes_written, Some(0));
    send.write_all(b"ab").await.unwrap();
    let mut chunks = Bytes::from_static(b"cd").chain(Bytes::from_static(b"ef"));
    send.write_all_buf(&mut chunks).await.unwrap();
    send.write_chunk(Bytes::from_static(b"ghi")).await.unwrap();
    let stats = send.stats().await.unwrap();
    assert_eq!(stats.bytes_written, Some(9));
    assert_eq!(stats.bytes_sent, None);
    assert_eq!(stats.bytes_acknowledged, None);
    send.finish().unwrap();
    assert_eq!(send.stats().await.unwrap(), stats);

    let mut recv = server.accept_uni().await.unwrap();
    assert_eq!(recv.stats().await.unwrap().bytes_read, Some(0));
    recv.read(&mut []).await.unwrap();
    assert_eq!(recv.stats().await.unwrap().bytes_read, Some(0));
    let first = recv.read_chunk(1).await.unwrap().unwrap();
    assert_eq!(first.as_ref(), b"a");
    assert_eq!(recv.stats().await.unwrap().bytes_read, Some(1));
    let mut rest = BytesMut::new();
    recv.read_all_buf(&mut rest).await.unwrap();
    assert_eq!(rest.as_ref(), b"bcdefghi");
    let stats = recv.stats().await.unwrap();
    assert_eq!(stats.bytes_read, Some(9));
    assert_eq!(stats.bytes_received, None);
    assert_eq!(recv.read(&mut [0; 1]).await.unwrap(), None);
    recv.stop(0);
    assert_eq!(recv.stats().await.unwrap(), stats);

    // Stream counters are independent of the session and each other.
    let (mut send, mut recv) = client.open_bi().await.unwrap();
    assert_eq!(send.stats().await.unwrap().bytes_written, Some(0));
    assert_eq!(recv.stats().await.unwrap().bytes_read, Some(0));
    send.write_all(b"request").await.unwrap();
    send.finish().unwrap();
    let (mut response, mut request) = server.accept_bi().await.unwrap();
    assert_eq!(response.stats().await.unwrap().bytes_written, Some(0));
    assert_eq!(request.stats().await.unwrap().bytes_read, Some(0));
    assert_eq!(request.read_all().await.unwrap().as_ref(), b"request");
    response.write_all(b"reply").await.unwrap();
    response.finish().unwrap();
    assert_eq!(recv.read_all().await.unwrap().as_ref(), b"reply");
    assert_eq!(send.stats().await.unwrap().bytes_written, Some(7));
    assert_eq!(recv.stats().await.unwrap().bytes_read, Some(5));
    assert_eq!(response.stats().await.unwrap().bytes_written, Some(5));
    assert_eq!(request.stats().await.unwrap().bytes_read, Some(7));
}
