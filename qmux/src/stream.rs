use web_transport_proto::VarInt;

/// Stream direction
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamDir {
    Bi,
    Uni,
}

/// Stream ID with direction encoding (QUIC-style)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StreamId(pub VarInt);

impl StreamId {
    pub fn new(id: u64, dir: StreamDir, is_server: bool) -> Self {
        let mut stream_id = id << 2;
        if dir == StreamDir::Uni {
            stream_id |= 0x02;
        }
        if is_server {
            stream_id |= 0x01;
        }
        StreamId(VarInt::try_from(stream_id).expect("stream ID too large"))
    }

    pub fn dir(&self) -> StreamDir {
        if self.0.into_inner() & 0x02 != 0 {
            StreamDir::Uni
        } else {
            StreamDir::Bi
        }
    }

    pub fn server_initiated(&self) -> bool {
        self.0.into_inner() & 0x01 != 0
    }

    pub fn can_recv(&self, is_server: bool) -> bool {
        match self.dir() {
            StreamDir::Uni => self.server_initiated() != is_server,
            StreamDir::Bi => true,
        }
    }

    pub fn can_send(&self, is_server: bool) -> bool {
        match self.dir() {
            StreamDir::Uni => self.server_initiated() == is_server,
            StreamDir::Bi => true,
        }
    }
}
