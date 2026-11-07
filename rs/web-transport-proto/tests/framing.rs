use std::{
    pin::Pin,
    task::{Context, Poll},
};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use web_transport_proto::{
    http, Capsule, ConnectRequest, ConnectResponse, Setting, Settings, VarInt,
};

struct Fragmented {
    bytes: Vec<u8>,
    position: usize,
    fragment: usize,
    pending: bool,
}

impl Fragmented {
    fn new(fragment: usize) -> Self {
        Self {
            bytes: Vec::new(),
            position: 0,
            fragment,
            pending: true,
        }
    }

    fn pause(&mut self, cx: &mut Context<'_>) -> bool {
        let pending = self.pending;
        self.pending = !pending;
        if pending {
            cx.waker().wake_by_ref();
        }
        pending
    }
}

impl AsyncRead for Fragmented {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.pause(cx) {
            return Poll::Pending;
        }
        let size = self
            .fragment
            .min(buf.remaining())
            .min(self.bytes.len() - self.position);
        buf.put_slice(&self.bytes[self.position..self.position + size]);
        self.position += size;
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for Fragmented {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if self.pause(cx) {
            return Poll::Pending;
        }
        let size = self.fragment.min(buf.len());
        self.bytes.extend_from_slice(&buf[..size]);
        Poll::Ready(Ok(size))
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn fragmented_connect_keeps_following_capsules_unread() {
    let capsule = Capsule::CloseWebTransportSession {
        code: 42,
        reason: "done".into(),
    };
    let request = ConnectRequest::new(
        "https://example.com/path?query=1"
            .parse::<url::Url>()
            .unwrap(),
    )
    .with_protocol("moq")
    .with_header(
        http::HeaderName::from_static("x-large"),
        http::HeaderValue::from_str(&"a".repeat(4096)).unwrap(),
    );
    let response = ConnectResponse::default().with_protocol("moq");
    for fragment in [1, 7, 4096] {
        let mut stream = Fragmented::new(fragment);
        request.write(&mut stream).await.unwrap();
        let size = stream.bytes.len();
        capsule.encode(&mut stream.bytes);
        let decoded = ConnectRequest::read(&mut stream).await.unwrap();
        assert_eq!(decoded.url, request.url);
        assert_eq!(decoded.headers, request.headers);
        assert_eq!(decoded.protocols, request.protocols);
        assert_eq!(stream.position, size);
        assert_eq!(
            Capsule::read(&mut stream).await.unwrap(),
            Some(capsule.clone())
        );

        let mut stream = Fragmented::new(fragment);
        response.write(&mut stream).await.unwrap();
        let size = stream.bytes.len();
        capsule.encode(&mut stream.bytes);
        let decoded = ConnectResponse::read(&mut stream).await.unwrap();
        assert_eq!(decoded.status, response.status);
        assert_eq!(decoded.protocol, response.protocol);
        assert_eq!(stream.position, size);
        assert_eq!(
            Capsule::read(&mut stream).await.unwrap(),
            Some(capsule.clone())
        );
    }
}

#[tokio::test]
async fn settings_lengths_cover_varint_widths_and_partial_io() {
    let mut settings = Settings::default();
    for id in [0, 63, 64, 16383, 16384, 1 << 30, VarInt::MAX.into_inner()] {
        settings.insert(
            Setting(VarInt::try_from(id).unwrap()),
            VarInt::try_from(id).unwrap(),
        );
    }
    settings.enable_webtransport(4);
    for fragment in [1, 7, 4096] {
        let mut stream = Fragmented::new(fragment);
        settings.write(&mut stream).await.unwrap();
        let mut expected = Vec::new();
        settings.encode(&mut expected);
        assert_eq!(stream.bytes, expected);
        let size = stream.bytes.len();
        stream.bytes.extend_from_slice(b"next frame");
        let decoded = Settings::read(&mut stream).await.unwrap();
        for (id, value) in settings.iter().filter(|(id, _)| !id.is_grease()) {
            assert_eq!(decoded.get(id), Some(value));
        }
        assert_eq!(stream.position, size);
    }
}
