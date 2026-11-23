use bytes::{Buf, BufMut};

use super::{VarInt, VarIntUnexpectedEnd};

// Sent as the first bytes of a unidirectional stream to identify the type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StreamUni(pub VarInt);

impl StreamUni {
    pub fn decode<B: Buf>(buf: &mut B) -> Result<Self, VarIntUnexpectedEnd> {
        Ok(StreamUni(VarInt::decode(buf)?))
    }

    pub fn encode<B: BufMut>(&self, buf: &mut B) {
        self.0.encode(buf)
    }

    pub fn is_grease(&self) -> bool {
        self.0.is_grease()
    }
}

macro_rules! streams_uni {
    {$($name:ident = $val:expr,)*} => {
        impl StreamUni {
            $(pub const $name: StreamUni = StreamUni(VarInt::from_u32($val));)*
        }
    }
}

streams_uni! {
    CONTROL = 0x00,
    PUSH = 0x01,
    QPACK_ENCODER = 0x02,
    QPACK_DECODER = 0x03,
    WEBTRANSPORT = 0x54,
}
