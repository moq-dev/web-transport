//! Allocation and boundary-copy measurements, also exercised by `just test`.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    hint::black_box,
    io::Cursor,
    pin::Pin,
    task::{Context, Poll},
    time::Instant,
};

use bytes::{buf::UninitSlice, BufMut, Bytes};
use tokio::io::AsyncWrite;
use web_transport_proto::{
    http, ConnectRequest, ConnectResponse, Frame, Setting, Settings, VarInt,
};

#[derive(Clone, Copy, Default)]
struct Allocations {
    allocations: usize,
    reallocations: usize,
    requested: usize,
}

thread_local! {
    static ALLOCATIONS: Cell<Option<Allocations>> = const { Cell::new(None) };
}

struct Allocator;

fn record(size: usize, reallocation: bool) {
    ALLOCATIONS.with(|cell| {
        if let Some(mut stats) = cell.get() {
            stats.allocations += usize::from(!reallocation);
            stats.reallocations += usize::from(reallocation);
            stats.requested += size;
            cell.set(Some(stats));
        }
    });
}

// SAFETY: every operation forwards its pointer and layout unchanged to System.
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size(), false);
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size(), false);
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(size, true);
        unsafe { System.realloc(ptr, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

struct Output<'a> {
    remaining: &'a mut [u8],
    largest_slice: usize,
}

// SAFETY: storage and advancement are delegated to the slice's BufMut impl.
unsafe impl BufMut for Output<'_> {
    fn remaining_mut(&self) -> usize {
        self.remaining.remaining_mut()
    }

    fn chunk_mut(&mut self) -> &mut UninitSlice {
        self.remaining.chunk_mut()
    }

    unsafe fn advance_mut(&mut self, cnt: usize) {
        unsafe { self.remaining.advance_mut(cnt) }
    }

    fn put_slice(&mut self, src: &[u8]) {
        self.largest_slice = self.largest_slice.max(src.len());
        self.remaining.put_slice(src);
    }
}

struct Writer(usize);

impl AsyncWrite for Writer {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        // Exercise partial writes without allocating another output buffer.
        let size = buf.len().min(7);
        black_box(&buf[..size]);
        self.0 += size;
        Poll::Ready(Ok(size))
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

fn measure(name: &str, wire_size: usize, mut operation: impl FnMut() -> usize) {
    // Warm up runtime and thread-local initialization outside the measurement.
    black_box(operation());
    ALLOCATIONS.with(|cell| cell.set(Some(Allocations::default())));
    let start = Instant::now();
    let mut observed = 0;
    const ITERATIONS: usize = 256;
    for _ in 0..ITERATIONS {
        observed = observed.max(black_box(operation()));
    }
    let elapsed = start.elapsed().as_nanos() / ITERATIONS as u128;
    let stats = ALLOCATIONS.with(|cell| cell.take().unwrap());
    println!(
        "{name},{wire_size},{ITERATIONS},{},{},{},{elapsed},{observed}",
        stats.allocations, stats.reallocations, stats.requested,
    );
}

fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    println!(
        "path,wire_bytes,iterations,allocations,reallocations,requested_bytes,ns_per_op,largest_encode_slice"
    );
    let mut storage = vec![0; 65560];

    macro_rules! paths {
        ($name:expr, $value:expr, $ty:ty) => {{
            let value = $value;
            let mut wire = Vec::new();
            value.encode(&mut wire).unwrap();
            measure(&format!("{}/encode", $name), wire.len(), || {
                let mut output = Output {
                    remaining: &mut storage,
                    largest_slice: 0,
                };
                value.encode(&mut output).unwrap();
                output.largest_slice
            });
            measure(&format!("{}/write", $name), wire.len(), || {
                let mut writer = Writer(0);
                runtime.block_on(value.write(&mut writer)).unwrap();
                assert_eq!(writer.0, wire.len());
                0
            });
            measure(&format!("{}/decode", $name), wire.len(), || {
                black_box(<$ty>::decode(&mut wire.as_slice()).unwrap());
                0
            });
            let owned = Bytes::copy_from_slice(&wire);
            measure(&format!("{}/decode_bytes", $name), wire.len(), || {
                black_box(<$ty>::decode(&mut owned.clone()).unwrap());
                0
            });
            measure(&format!("{}/read", $name), wire.len(), || {
                black_box(
                    runtime
                        .block_on(<$ty>::read(&mut Cursor::new(&wire)))
                        .unwrap(),
                );
                0
            });
        }};
    }

    for (fields, value_size) in [(0, 0), (8, 128), (1, 12000)] {
        let mut request = ConnectRequest::new(
            "https://example.com/path?query=1"
                .parse::<url::Url>()
                .unwrap(),
        )
        .with_protocol("moq");
        for i in 0..fields {
            request.headers.append(
                http::HeaderName::from_bytes(format!("x-field-{i}").as_bytes()).unwrap(),
                http::HeaderValue::from_str(&"a".repeat(value_size)).unwrap(),
            );
        }
        paths!(
            &format!("request/{fields}x{value_size}"),
            request,
            ConnectRequest
        );
    }
    paths!(
        "response",
        ConnectResponse::default().with_protocol("moq"),
        ConnectResponse
    );

    // 1024 Huffman-coded 'a' bytes, using RFC 7541 Appendix B's five-bit code.
    let mut section = vec![0, 0, 0xd9, 0x26];
    section.extend_from_slice(b"x-data");
    section.extend_from_slice(&[0xff, 0x81, 0x04]); // Huffman flag and length 640.
    for _ in 0..128 {
        section.extend_from_slice(&[0x18, 0xc6, 0x31, 0x8c, 0x63]);
    }
    let mut wire = Vec::new();
    Frame::HEADERS.encode(&mut wire);
    VarInt::from_u32(section.len() as u32).encode(&mut wire);
    wire.extend_from_slice(&section);
    measure("huffman/decode", wire.len(), || {
        black_box(ConnectResponse::decode(&mut wire.as_slice()).unwrap());
        0
    });
    measure("huffman/read", wire.len(), || {
        black_box(
            runtime
                .block_on(ConnectResponse::read(&mut Cursor::new(&wire)))
                .unwrap(),
        );
        0
    });

    for count in [0, 64] {
        let mut settings = Settings::default();
        settings.enable_webtransport(4);
        for i in 0..count {
            settings.insert(
                Setting(VarInt::from_u32(0x1000 + i)),
                VarInt::from_u32(1 << 20),
            );
        }
        let mut wire = Vec::new();
        settings.encode(&mut wire);
        measure(&format!("settings/{count}/encode"), wire.len(), || {
            let mut output = Output {
                remaining: &mut storage,
                largest_slice: 0,
            };
            settings.encode(&mut output);
            output.largest_slice
        });
        measure(&format!("settings/{count}/write"), wire.len(), || {
            let mut writer = Writer(0);
            runtime.block_on(settings.write(&mut writer)).unwrap();
            assert_eq!(writer.0, wire.len());
            0
        });
        measure(&format!("settings/{count}/decode"), wire.len(), || {
            black_box(Settings::decode(&mut wire.as_slice()).unwrap());
            0
        });
        measure(&format!("settings/{count}/read"), wire.len(), || {
            black_box(
                runtime
                    .block_on(Settings::read(&mut Cursor::new(&wire)))
                    .unwrap(),
            );
            0
        });
    }
}
