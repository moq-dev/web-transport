//! Scaling of default receive initialization across independent streams, buffer
//! allocations, and fragment sizes. Run with `cargo bench -p web-transport-trait`.
use std::{
    hint::black_box,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
    time::Instant,
};

use bytes::BytesMut;
use web_transport_trait::poll::RecvStream;

#[derive(Debug)]
struct Error;
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("benchmark error")
    }
}
impl std::error::Error for Error {}
impl web_transport_trait::Error for Error {
    fn session_error(&self) -> Option<(u32, String)> {
        None
    }
}

struct Reader {
    fragment: usize,
    initialized: usize,
}
impl RecvStream for Reader {
    type Error = Error;

    fn poll_read(
        &mut self,
        _cx: &mut Context<'_>,
        dst: &mut [u8],
    ) -> Poll<Result<Option<usize>, Error>> {
        // A safe reader can inspect its entire destination. Keep initialization
        // observable so the optimizer cannot discard it as an unused write.
        black_box(&*dst);
        self.initialized += dst.len();
        let size = self.fragment.min(dst.len());
        dst[..size].fill(1);
        Poll::Ready(Ok(Some(size)))
    }
    fn stop(&mut self, _code: u32) {}
    fn poll_closed(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Poll::Ready(Ok(()))
    }
}

struct Noop;
impl Wake for Noop {
    fn wake(self: Arc<Self>) {}
}

fn main() {
    let waker = Waker::from(Arc::new(Noop));
    let mut cx = Context::from_waker(&waker);
    println!("streams,capacity,fragment,ns_per_read,initialized_per_read");
    for streams in [1usize, 16] {
        for capacity in [64 * 1024, 1024 * 1024, 16 * 1024 * 1024] {
            for fragment in [1, 1024] {
                let mut readers: Vec<_> = (0..streams)
                    .map(|_| {
                        (
                            Reader {
                                fragment,
                                initialized: 0,
                            },
                            BytesMut::with_capacity(capacity),
                        )
                    })
                    .collect();
                let start = Instant::now();
                for _ in 0..128 {
                    for (reader, buf) in &mut readers {
                        assert!(matches!(
                            reader.poll_read_buf(&mut cx, buf),
                            Poll::Ready(Ok(Some(_)))
                        ));
                    }
                }
                let ns_per_read = start.elapsed().as_nanos() / (streams * 128) as u128;
                let initialized: usize = readers.iter().map(|(reader, _)| reader.initialized).sum();
                println!(
                    "{streams},{capacity},{fragment},{ns_per_read},{}",
                    initialized / (streams * 128)
                );
                black_box(readers);
            }
        }
    }
}
