//! Byte-level wire-format fixtures for the legacy `webtransport` and QMux00 formats.
//!
//! Each test hard-codes the exact bytes a peer would put on the wire and verifies:
//!   1. The current decoder parses them into the expected `Frame` value.
//!   2. The current encoder produces the same bytes from the same `Frame` value.
//!
//! These are the regression guards we need before deploying alongside peers
//! that haven't picked up the QMux01 changes. If any of these fixtures drift,
//! we've broken wire compatibility — independent of any test that talks to
//! itself end-to-end (which would mask a symmetric breakage).
//!
//! Varint reference (see `web-transport-proto`):
//!
//!   - 1-byte: `0b00xxxxxx`, payload is the low 6 bits
//!   - 2-byte: `0b01xxxxxx xxxxxxxx`, payload is the low 14 bits
//!   - 4-byte: `0b10...`, 30-bit payload
//!   - 8-byte: `0b11...`, 62-bit payload
//!
//! So `100 = 0x64` needs a 2-byte varint: `0x40 0x64`. And `1024 = 0x0400`
//! encodes as `0x44 0x00`.

use bytes::Bytes;
use qmux::proto::{ConnectionClose, Frame, ResetStream, StopSending, Stream};
use qmux::{Error, StreamId, Version};
use web_transport_proto::VarInt;

/// Round-trip helper: hard-coded bytes ↔ expected frame, both directions.
fn assert_round_trip(version: Version, bytes: &[u8], expected: &Frame) {
    // Decode side: the bytes on the wire become the expected frame.
    let decoded = Frame::decode(Bytes::copy_from_slice(bytes), version)
        .expect("decode succeeds")
        .expect("decoder returns a frame (not an ignored frame type)");
    assert_frames_eq(&decoded, expected, version);

    // Encode side: the expected frame produces the exact same bytes.
    let encoded = expected.encode(version).expect("encode succeeds");
    assert_eq!(
        encoded.as_ref(),
        bytes,
        "encoding {expected:?} for {version:?} must produce identical bytes"
    );
}

/// `Frame` doesn't implement `PartialEq`, so spell out the field-level checks
/// per variant. Limited to the variants used in these tests.
fn assert_frames_eq(got: &Frame, want: &Frame, version: Version) {
    match (got, want) {
        (Frame::Stream(a), Frame::Stream(b)) => {
            assert_eq!(a.id.0.into_inner(), b.id.0.into_inner(), "stream id");
            assert_eq!(a.data.as_ref(), b.data.as_ref(), "stream data");
            assert_eq!(a.fin, b.fin, "stream fin");
        }
        (Frame::ResetStream(a), Frame::ResetStream(b)) => {
            assert_eq!(a.id.0.into_inner(), b.id.0.into_inner(), "reset id");
            assert_eq!(a.code.into_inner(), b.code.into_inner(), "reset code");
            // WebTransport's decode_wt synthesizes final_size=0 (the wire format omits it),
            // so only compare when the version actually carries it.
            if version != Version::WebTransport {
                assert_eq!(a.final_size, b.final_size, "reset final_size");
            }
        }
        (Frame::StopSending(a), Frame::StopSending(b)) => {
            assert_eq!(a.id.0.into_inner(), b.id.0.into_inner(), "stop id");
            assert_eq!(a.code.into_inner(), b.code.into_inner(), "stop code");
        }
        (Frame::ConnectionClose(a), Frame::ConnectionClose(b)) => {
            assert_eq!(a.code.into_inner(), b.code.into_inner(), "close code");
            assert_eq!(a.reason, b.reason, "close reason");
        }
        (Frame::MaxData(a), Frame::MaxData(b)) => assert_eq!(a, b, "max_data"),
        (
            Frame::MaxStreamData {
                id: a_id,
                max: a_max,
            },
            Frame::MaxStreamData {
                id: b_id,
                max: b_max,
            },
        ) => {
            assert_eq!(
                a_id.0.into_inner(),
                b_id.0.into_inner(),
                "max_stream_data id"
            );
            assert_eq!(a_max, b_max, "max_stream_data max");
        }
        (Frame::MaxStreamsBidi(a), Frame::MaxStreamsBidi(b)) => {
            assert_eq!(a, b, "max_streams_bidi")
        }
        (Frame::MaxStreamsUni(a), Frame::MaxStreamsUni(b)) => assert_eq!(a, b, "max_streams_uni"),
        (Frame::DataBlocked(a), Frame::DataBlocked(b)) => assert_eq!(a, b, "data_blocked"),
        (
            Frame::StreamDataBlocked {
                id: a_id,
                limit: a_lim,
            },
            Frame::StreamDataBlocked {
                id: b_id,
                limit: b_lim,
            },
        ) => {
            assert_eq!(
                a_id.0.into_inner(),
                b_id.0.into_inner(),
                "stream_data_blocked id"
            );
            assert_eq!(a_lim, b_lim, "stream_data_blocked limit");
        }
        (Frame::StreamsBlockedBidi(a), Frame::StreamsBlockedBidi(b)) => {
            assert_eq!(a, b, "streams_blocked_bidi")
        }
        (Frame::StreamsBlockedUni(a), Frame::StreamsBlockedUni(b)) => {
            assert_eq!(a, b, "streams_blocked_uni")
        }
        (Frame::Datagram(a), Frame::Datagram(b)) => {
            assert_eq!(a.data.as_ref(), b.data.as_ref(), "datagram payload")
        }
        (a, b) => panic!("frame variants don't match: got {a:?}, want {b:?}"),
    }
}

fn sid(v: u64) -> StreamId {
    StreamId(VarInt::try_from(v).unwrap())
}

fn code(v: u64) -> VarInt {
    VarInt::try_from(v).unwrap()
}

// --------------------------------------------------------------------------
// WebTransport wire format (HTTP/3 WT capsules)
// --------------------------------------------------------------------------

#[test]
fn webtransport_stream_no_fin() {
    // 0x08 = STREAM, id varint = 0x04, payload = "hi" (rest of buffer).
    let bytes = [0x08, 0x04, b'h', b'i'];
    let frame = Frame::Stream(Stream {
        id: sid(4),
        data: Bytes::from_static(b"hi"),
        fin: false,
    });
    assert_round_trip(Version::WebTransport, &bytes, &frame);
}

#[test]
fn webtransport_stream_fin() {
    // 0x09 = STREAM | FIN.
    let bytes = [0x09, 0x08, b'b', b'y', b'e'];
    let frame = Frame::Stream(Stream {
        id: sid(8),
        data: Bytes::from_static(b"bye"),
        fin: true,
    });
    assert_round_trip(Version::WebTransport, &bytes, &frame);
}

#[test]
fn webtransport_reset_stream() {
    // 0x04 + id(=4) + code(=42). WebTransport carries no final_size.
    let bytes = [0x04, 0x04, 0x2a];
    let frame = Frame::ResetStream(ResetStream {
        id: sid(4),
        code: code(42),
        final_size: 0,
    });
    assert_round_trip(Version::WebTransport, &bytes, &frame);
}

#[test]
fn webtransport_stop_sending() {
    // 0x05 + id(=4) + code(=42).
    let bytes = [0x05, 0x04, 0x2a];
    let frame = Frame::StopSending(StopSending {
        id: sid(4),
        code: code(42),
    });
    assert_round_trip(Version::WebTransport, &bytes, &frame);
}

#[test]
fn webtransport_connection_close() {
    // 0x1d + code(=42) + reason("bye") as the rest of the buffer.
    let bytes = [0x1d, 0x2a, b'b', b'y', b'e'];
    let frame = Frame::ConnectionClose(ConnectionClose {
        code: code(42),
        reason: "bye".to_string(),
    });
    assert_round_trip(Version::WebTransport, &bytes, &frame);
}

// --------------------------------------------------------------------------
// QMux draft-00 wire format (QUIC-style framing on a reliable byte stream)
// --------------------------------------------------------------------------

#[test]
fn qmux00_stream_with_len_no_fin() {
    // 0x0a = STREAM | LEN (no OFF, no FIN), id=4, len=2, "hi".
    let bytes = [0x0a, 0x04, 0x02, b'h', b'i'];
    let frame = Frame::Stream(Stream {
        id: sid(4),
        data: Bytes::from_static(b"hi"),
        fin: false,
    });
    assert_round_trip(Version::QMux00, &bytes, &frame);
}

#[test]
fn qmux00_stream_with_len_and_fin() {
    // 0x0b = STREAM | LEN | FIN, id=8, len=3, "bye".
    let bytes = [0x0b, 0x08, 0x03, b'b', b'y', b'e'];
    let frame = Frame::Stream(Stream {
        id: sid(8),
        data: Bytes::from_static(b"bye"),
        fin: true,
    });
    assert_round_trip(Version::QMux00, &bytes, &frame);
}

#[test]
fn qmux00_reset_stream() {
    // 0x04 + id(=4) + code(=42) + final_size(=128, 2-byte varint = 0x40 0x80).
    let bytes = [0x04, 0x04, 0x2a, 0x40, 0x80];
    let frame = Frame::ResetStream(ResetStream {
        id: sid(4),
        code: code(42),
        final_size: 128,
    });
    assert_round_trip(Version::QMux00, &bytes, &frame);
}

#[test]
fn qmux00_stop_sending() {
    // 0x05 + id(=4) + code(=42).
    let bytes = [0x05, 0x04, 0x2a];
    let frame = Frame::StopSending(StopSending {
        id: sid(4),
        code: code(42),
    });
    assert_round_trip(Version::QMux00, &bytes, &frame);
}

#[test]
fn qmux01_datagram() {
    // 0x31 = DATAGRAM | LEN; len=2, payload "hi". Datagrams are a QMux01 feature;
    // we always emit the length-prefixed form.
    let bytes = [0x31, 0x02, b'h', b'i'];
    let frame = Frame::Datagram(Bytes::from_static(b"hi").into());
    assert_round_trip(Version::QMux01, &bytes, &frame);
}

#[test]
fn datagram_rejected_on_non_qmux01() {
    // Datagrams are a QMux01-only frame; encoding one for an older wire version
    // must fail rather than emit draft-01 bytes onto a draft-00 / WebTransport
    // session that can't interpret them.
    for version in [Version::WebTransport, Version::QMux00] {
        let err = Frame::Datagram(Bytes::from_static(b"hi").into())
            .encode(version)
            .expect_err("datagram must not encode on non-QMux01");
        assert!(matches!(err, Error::InvalidFrameType(_)), "got {err:?}");
    }
}

#[test]
fn qmux_datagram_no_length_decodes() {
    // A peer may use the no-length form (0x30 + payload), where the payload runs
    // to the end of the record. We never emit it, but must decode it.
    let bytes = [0x30, b'h', b'i'];
    let decoded = Frame::decode(Bytes::copy_from_slice(&bytes), Version::QMux01)
        .expect("decode succeeds")
        .expect("datagram is not an ignored frame");
    match decoded {
        Frame::Datagram(dg) => {
            assert_eq!(dg.data.as_ref(), b"hi");
            // The no-length form is preserved so the size check can use its
            // true (length-varint-free) frame size.
            assert!(!dg.length_prefixed, "0x30 form must decode as no-length");
        }
        other => panic!("expected datagram, got {other:?}"),
    }
}

#[test]
fn record_datagram_then_stream_decodes_both() {
    // The length-prefixed datagram (0x31) must stop consumption at its payload
    // boundary so a following frame in the same record still decodes — the exact
    // reason we always emit 0x31 rather than the no-length 0x30 form.
    let datagram = Frame::Datagram(Bytes::from_static(b"hi").into())
        .encode(Version::QMux01)
        .unwrap();
    let stream = Frame::Stream(Stream {
        id: sid(4),
        data: Bytes::from_static(b"bye"),
        fin: false,
    })
    .encode(Version::QMux01)
    .unwrap();

    let mut record = Vec::new();
    record.extend_from_slice(&datagram);
    record.extend_from_slice(&stream);

    let frames = Frame::decode_record(Bytes::from(record)).expect("record decodes");
    assert_eq!(frames.len(), 2, "both frames decode");
    match &frames[0] {
        Frame::Datagram(dg) => assert_eq!(dg.data.as_ref(), b"hi"),
        other => panic!("expected datagram, got {other:?}"),
    }
    match &frames[1] {
        Frame::Stream(s) => {
            assert_eq!(s.id.0.into_inner(), 4);
            assert_eq!(s.data.as_ref(), b"bye");
        }
        other => panic!("expected stream, got {other:?}"),
    }
}

// --------------------------------------------------------------------------
// QMux draft-02: RESET_STREAM_AT + wire compatibility with draft-01
// --------------------------------------------------------------------------

#[test]
fn qmux02_reset_stream_at_decodes_as_reset() {
    // 0x24 RESET_STREAM_AT: id=4, code=42, final_size=128 (0x40 0x80),
    // reliable_size=64 (0x40 0x40). QMux runs on a reliable, ordered transport, so
    // this decodes to a plain reset carrying final_size; reliable_size is validated
    // (<= final_size) and dropped.
    let bytes = [0x24, 0x04, 0x2a, 0x40, 0x80, 0x40, 0x40];
    let decoded = Frame::decode(Bytes::copy_from_slice(&bytes), Version::QMux02)
        .expect("decode succeeds")
        .expect("reset_stream_at is not an ignored frame");
    match decoded {
        Frame::ResetStream(r) => {
            assert_eq!(r.id.0.into_inner(), 4, "reset id");
            assert_eq!(r.code.into_inner(), 42, "reset code");
            assert_eq!(r.final_size, 128, "reset final_size");
        }
        other => panic!("expected reset_stream, got {other:?}"),
    }
}

#[test]
fn qmux02_reset_stream_at_reliable_over_final_rejected() {
    // reliable_size (200 = 0x40 0xc8) > final_size (128 = 0x40 0x80) is a
    // FRAME_ENCODING_ERROR per draft-ietf-quic-reliable-stream-reset.
    let bytes = [0x24, 0x04, 0x2a, 0x40, 0x80, 0x40, 0xc8];
    let err = Frame::decode(Bytes::copy_from_slice(&bytes), Version::QMux02)
        .expect_err("reliable_size > final_size must be rejected");
    assert!(matches!(err, Error::FrameEncoding), "got {err:?}");
}

#[test]
fn qmux02_reset_stream_at_in_record() {
    // The same frame decodes through the record path (draft-01+ framing), stopping
    // at its own boundary so a following frame in the record still decodes.
    let reset_at = [0x24u8, 0x04, 0x2a, 0x40, 0x80, 0x40, 0x40];
    let stream = Frame::Stream(Stream {
        id: sid(8),
        data: Bytes::from_static(b"bye"),
        fin: false,
    })
    .encode(Version::QMux02)
    .unwrap();

    let mut record = reset_at.to_vec();
    record.extend_from_slice(&stream);

    let frames = Frame::decode_record(Bytes::from(record)).expect("record decodes");
    assert_eq!(frames.len(), 2, "both frames decode");
    match &frames[0] {
        Frame::ResetStream(r) => assert_eq!(r.final_size, 128),
        other => panic!("expected reset_stream, got {other:?}"),
    }
    match &frames[1] {
        Frame::Stream(s) => assert_eq!(s.data.as_ref(), b"bye"),
        other => panic!("expected stream, got {other:?}"),
    }
}

#[test]
fn qmux02_wire_matches_qmux01() {
    // Draft-02 shares the draft-01 record wire format: the same frames must encode
    // byte-for-byte identically under both versions.
    let frames: Vec<Frame> = vec![
        Frame::Stream(Stream {
            id: sid(4),
            data: Bytes::from_static(b"hi"),
            fin: true,
        }),
        Frame::MaxData(1024),
        Frame::Datagram(Bytes::from_static(b"hi").into()),
    ];
    for frame in &frames {
        let a = frame.encode(Version::QMux01).unwrap();
        let b = frame.encode(Version::QMux02).unwrap();
        assert_eq!(
            a.as_ref(),
            b.as_ref(),
            "qmux-02 must match qmux-01 for {frame:?}"
        );
    }
}

#[test]
fn qmux00_application_close() {
    // 0x1d (APPLICATION_CLOSE) + code(=42) + frame_type(=0) + reason_len(=3) + "bye".
    let bytes = [0x1d, 0x2a, 0x00, 0x03, b'b', b'y', b'e'];
    let frame = Frame::ConnectionClose(ConnectionClose {
        code: code(42),
        reason: "bye".to_string(),
    });
    assert_round_trip(Version::QMux00, &bytes, &frame);
}

#[test]
fn qmux00_max_data() {
    // 0x10 + max(=1024, 2-byte varint = 0x44 0x00).
    let bytes = [0x10, 0x44, 0x00];
    let frame = Frame::MaxData(1024);
    assert_round_trip(Version::QMux00, &bytes, &frame);
}

#[test]
fn qmux00_max_stream_data() {
    // 0x11 + id(=4) + max(=1024).
    let bytes = [0x11, 0x04, 0x44, 0x00];
    let frame = Frame::MaxStreamData {
        id: sid(4),
        max: 1024,
    };
    assert_round_trip(Version::QMux00, &bytes, &frame);
}

#[test]
fn qmux00_max_streams_bidi() {
    // 0x12 + max(=100, 2-byte varint = 0x40 0x64).
    let bytes = [0x12, 0x40, 0x64];
    let frame = Frame::MaxStreamsBidi(100);
    assert_round_trip(Version::QMux00, &bytes, &frame);
}

#[test]
fn qmux00_max_streams_uni() {
    // 0x13 + max(=100).
    let bytes = [0x13, 0x40, 0x64];
    let frame = Frame::MaxStreamsUni(100);
    assert_round_trip(Version::QMux00, &bytes, &frame);
}

#[test]
fn qmux00_data_blocked() {
    // 0x14 + limit(=1024).
    let bytes = [0x14, 0x44, 0x00];
    let frame = Frame::DataBlocked(1024);
    assert_round_trip(Version::QMux00, &bytes, &frame);
}

#[test]
fn qmux00_stream_data_blocked() {
    // 0x15 + id(=4) + limit(=1024).
    let bytes = [0x15, 0x04, 0x44, 0x00];
    let frame = Frame::StreamDataBlocked {
        id: sid(4),
        limit: 1024,
    };
    assert_round_trip(Version::QMux00, &bytes, &frame);
}

#[test]
fn qmux00_streams_blocked_bidi() {
    let bytes = [0x16, 0x40, 0x64];
    let frame = Frame::StreamsBlockedBidi(100);
    assert_round_trip(Version::QMux00, &bytes, &frame);
}

#[test]
fn qmux00_streams_blocked_uni() {
    let bytes = [0x17, 0x40, 0x64];
    let frame = Frame::StreamsBlockedUni(100);
    assert_round_trip(Version::QMux00, &bytes, &frame);
}

#[test]
fn qmux00_transport_parameters_initial_max_data_only() {
    // Frame type QX_TRANSPORT_PARAMETERS = 0x3f5153300d0a0d0a (8-byte varint).
    // Encoded varint: 0xff 0x51 0x53 0x30 0x0d 0x0a 0x0d 0x0a — top 2 bits replaced with
    // the 8-byte tag (0b11), low 62 bits unchanged.
    //
    // Payload: one parameter with id=0x04 (initial_max_data), value=1024.
    //   id_varint  = 0x04          (1 byte)
    //   len_varint = 0x02          (encoded value is 2 bytes long)
    //   value      = 0x44 0x00     (2-byte varint for 1024)
    // Payload length = 4 bytes → payload_len_varint = 0x04.
    let bytes = [
        // Frame type
        0xff, 0x51, 0x53, 0x30, 0x0d, 0x0a, 0x0d, 0x0a, // Payload length
        0x04, // Payload: id, val_len, value(2-byte varint for 1024)
        0x04, 0x02, 0x44, 0x00,
    ];

    let decoded = Frame::decode(Bytes::copy_from_slice(&bytes), Version::QMux00)
        .unwrap()
        .unwrap();
    let mut params = match decoded {
        Frame::TransportParameters(p) => {
            assert_eq!(p.initial_max_data, 1024);
            // Every other value defaults to 0 except max_record_size, which
            // decode() seeds with DEFAULT_MAX_RECORD_SIZE per draft-01.
            assert_eq!(p.initial_max_stream_data_bidi_local, 0);
            assert_eq!(p.initial_max_streams_bidi, 0);
            assert_eq!(p.max_record_size, qmux::proto::DEFAULT_MAX_RECORD_SIZE);
            p
        }
        other => panic!("expected TransportParameters, got {other:?}"),
    };

    // Re-encoding must round-trip byte-for-byte. Zero out max_record_size first:
    // a peer that didn't include it wouldn't have, and the encoder skips zero-valued params,
    // so this matches what would actually have gone on the wire.
    params.max_record_size = 0;
    let encoded = Frame::TransportParameters(params)
        .encode(Version::QMux00)
        .unwrap();
    assert_eq!(encoded.as_ref(), bytes);
}

/// Specifically guard against the bug class the WebSocket fix was guarding against:
/// QMux00 must NOT prepend a record-size varint to any encoded frame, regardless
/// of frame type.
#[test]
fn qmux00_encoding_has_no_record_size_prefix() {
    let cases: Vec<Frame> = vec![
        Frame::Stream(Stream {
            id: sid(4),
            data: Bytes::from_static(b"hi"),
            fin: false,
        }),
        Frame::MaxData(1024),
        Frame::ConnectionClose(ConnectionClose {
            code: code(42),
            reason: "bye".to_string(),
        }),
    ];
    for frame in &cases {
        let bytes = frame.encode(Version::QMux00).unwrap();
        // The first byte must match the encoder's frame-type tag directly — not a size varint.
        match frame {
            Frame::Stream(_) => assert_eq!(bytes[0], 0x0a, "stream without fin = type 0x0a"),
            Frame::MaxData(_) => assert_eq!(bytes[0], 0x10, "max_data = type 0x10"),
            Frame::ConnectionClose(_) => {
                assert_eq!(bytes[0], 0x1d, "application_close = type 0x1d")
            }
            _ => unreachable!(),
        }
    }
}
