//! Verify that Tokio I/O shares the counters used by the async trait.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use web_transport_trait::{RecvStream, SendStream, Session};

pub async fn payload_stats<S: Session>(client: &S, server: &S)
where
    S::SendStream: AsyncWrite + Unpin,
    S::RecvStream: AsyncRead + Unpin,
{
    let mut send = client.open_uni().await.unwrap();
    AsyncWriteExt::write_all(&mut send, b"tokio").await.unwrap();
    assert_eq!(send.stats().await.unwrap().bytes_written, Some(5));
    SendStream::write_all(&mut send, b"+trait").await.unwrap();
    send.finish().unwrap();
    assert_eq!(send.stats().await.unwrap().bytes_written, Some(11));
    let mut recv = server.accept_uni().await.unwrap();
    let mut first = [0; 5];
    AsyncReadExt::read_exact(&mut recv, &mut first)
        .await
        .unwrap();
    assert_eq!(&first, b"tokio");
    assert_eq!(recv.stats().await.unwrap().bytes_read, Some(5));
    assert_eq!(
        RecvStream::read_all(&mut recv).await.unwrap().as_ref(),
        b"+trait"
    );
    assert_eq!(recv.stats().await.unwrap().bytes_read, Some(11));
}
