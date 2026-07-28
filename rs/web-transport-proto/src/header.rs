use bytes::BufMut;

/// A short prefix written in front of every WebTransport stream and datagram,
/// held inline so it never touches the heap.
///
/// A header is at most two [`VarInt`](crate::VarInt)s — a stream or frame type
/// plus the session ID — and a varint encodes to at most 8 bytes. Sixteen bytes
/// covers it, which makes this `Copy`.
///
/// That matters on the open path: a backend hands its cached header to the future
/// behind `poll_open_uni`/`poll_open_bi`, and that future has to own what it holds,
/// so a `Vec` here would mean one allocation per stream opened — defeating the point
/// of caching the encoded bytes in the first place.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Header {
    buf: [u8; Self::MAX],
    len: usize,
}

impl Header {
    /// Two varints, each at most 8 bytes.
    pub const MAX: usize = 16;

    /// Encode a header by writing into the provided buffer.
    ///
    /// Panics if more than [`MAX`](Self::MAX) bytes are written, which would mean a
    /// caller wrote something other than the two varints this is sized for.
    pub fn encode(write: impl FnOnce(&mut &mut [u8])) -> Self {
        let mut buf = [0u8; Self::MAX];
        let mut rest = &mut buf[..];
        write(&mut rest);
        let len = Self::MAX - rest.remaining_mut();

        Self { buf, len }
    }

    /// The encoded bytes.
    pub fn as_slice(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    /// Whether this header encodes to nothing, as it does for raw QUIC.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The number of encoded bytes.
    pub fn len(&self) -> usize {
        self.len
    }
}

impl std::ops::Deref for Header {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl AsRef<[u8]> for Header {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VarInt;

    #[test]
    fn round_trips_two_varints() {
        let header = Header::encode(|w| {
            VarInt::from_u32(0x41).encode(w);
            VarInt::from_u32(0x1234).encode(w);
        });

        let mut expected = Vec::new();
        VarInt::from_u32(0x41).encode(&mut expected);
        VarInt::from_u32(0x1234).encode(&mut expected);

        assert_eq!(header.as_slice(), &expected[..]);
    }

    #[test]
    fn empty_by_default() {
        assert!(Header::default().is_empty());
        assert!(Header::encode(|_| {}).is_empty());
    }

    /// The size that justifies the inline buffer: two maximal varints.
    #[test]
    fn two_maximal_varints_fit() {
        let max = VarInt::from_u64(2u64.pow(62) - 1).unwrap();
        let header = Header::encode(|w| {
            max.encode(w);
            max.encode(w);
        });

        assert_eq!(header.len(), Header::MAX);
    }
}
