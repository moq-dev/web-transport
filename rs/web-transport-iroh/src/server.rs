use iroh::endpoint::Connection;
use web_transport_proto::{ConnectRequest, ConnectResponse};

use crate::{Connecting, ServerError, Session, Settings};

/// A QUIC-only WebTransport handshake, awaiting server decision.
pub struct QuicRequest {
    conn: Connection,
}

/// An H3 WebTransport handshake, SETTINGS exchanged and CONNECT accepted,
/// awaiting server decision (respond OK / reject).
#[derive(Debug)]
pub struct H3Request {
    conn: Connection,
    settings: Settings,
    connect: Connecting,
}

impl QuicRequest {
    /// Accept a new QUIC-only WebTransport session from a client.
    pub fn accept(conn: Connection) -> Self {
        Self { conn }
    }

    /// Returns the underlying QUIC connection.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Accept the session.
    pub fn ok(self) -> Session {
        Session::raw(self.conn)
    }

    /// Reject the session.
    pub fn close(self, status: http::StatusCode) {
        self.conn
            .close(status.as_u16().into(), status.as_str().as_bytes());
    }
}

impl H3Request {
    /// Accept a new H3 WebTransport session from a client.
    pub async fn accept(conn: Connection) -> Result<Self, ServerError> {
        // Perform the H3 handshake by sending/receiving SETTINGS frames.
        let settings = Settings::connect(&conn).await?;

        // Accept the CONNECT request but don't send a response yet.
        let connect = Connecting::accept(&conn).await?;

        Ok(Self {
            conn,
            settings,
            connect,
        })
    }

    /// Returns the underlying QUIC connection.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Accept the session with a default 200 OK response.
    pub async fn ok(self) -> Result<Session, ServerError> {
        self.respond(ConnectResponse::OK).await
    }

    /// Reply to the session with the given response, usually 200 OK.
    ///
    /// [`ConnectResponse::with_protocol`] can be used to select a subprotocol.
    pub async fn respond(
        self,
        response: impl Into<ConnectResponse>,
    ) -> Result<Session, ServerError> {
        let response = response.into();
        let connect = self.connect.respond(response).await?;
        Ok(Session::new_h3(self.conn, self.settings, connect))
    }

    /// Reject the session with the given status code.
    pub async fn reject(self, status: http::StatusCode) -> Result<(), ServerError> {
        self.connect.reject(status).await?;
        Ok(())
    }

    /// Returns the [`ConnectRequest`] sent by the client.
    pub fn request(&self) -> &ConnectRequest {
        &self.connect
    }
}

impl core::ops::Deref for H3Request {
    type Target = ConnectRequest;

    fn deref(&self) -> &Self::Target {
        &self.connect
    }
}
