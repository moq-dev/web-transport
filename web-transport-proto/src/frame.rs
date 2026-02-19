use bytes::{Buf, BufMut};

use crate::{VarInt, VarIntUnexpectedEnd};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Frame(pub VarInt);

impl Frame {
    pub fn decode<B: Buf>(buf: &mut B) -> Result<Self, VarIntUnexpectedEnd> {
        let typ = VarInt::decode(buf)?;
        Ok(Frame(typ))
    }

    pub fn encode<B: BufMut>(&self, buf: &mut B) {
        self.0.encode(buf)
    }

    pub fn is_grease(&self) -> bool {
        let val = self.0.into_inner();
        if val < 0x21 {
            return false;
        }

        #[allow(unknown_lints, clippy::manual_is_multiple_of)]
        {
            (val - 0x21) % 0x1f == 0
        }
    }

    pub fn read<B: Buf>(
        buf: &mut B,
    ) -> Result<(Frame, bytes::buf::Take<&mut B>), VarIntUnexpectedEnd> {
        let typ = Frame::decode(buf)?;
        let size = VarInt::decode(buf)?;

        let mut limit = Buf::take(buf, size.into_inner() as usize);
        if limit.remaining() < limit.limit() {
            return Err(VarIntUnexpectedEnd);
        }

        // Try again if this is a GREASE frame we need to ignore
        if typ.is_grease() {
            limit.advance(limit.limit());
            return Self::read(limit.into_inner());
        }

        Ok((typ, limit))
    }
}

macro_rules! frames {
    {$($name:ident = $val:expr,)*} => {
        impl Frame {
            $(pub const $name: Frame = Frame(VarInt::from_u32($val));)*
        }
    }
}

/// Maximum size of a frame payload (64 KiB).
///
/// This bounds the allocation when reading frames from an async stream,
/// preventing a malicious peer from triggering huge allocations via a
/// crafted frame-length field.
pub(crate) const MAX_FRAME_SIZE: u64 = 65536;

// Sent at the start of a bidirectional stream.
frames! {
    DATA = 0x00,
    HEADERS = 0x01,
    SETTINGS = 0x04,
    WEBTRANSPORT = 0x41,
}
