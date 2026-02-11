use crate::{ez, h3, proto::ConnectResponse, Connection, ServerError};

/// A mostly complete WebTransport handshake, just awaiting the server's decision on whether to accept or reject the session based on the URL.
pub struct Request {
    conn: ez::Connection,
    settings: h3::Settings,
    connect: h3::Connecting,
}

impl Request {
    /// Accept a new WebTransport session from a client.
    pub async fn accept(conn: ez::Connection) -> Result<Self, ServerError> {
        // Perform the H3 handshake by sending/reciving SETTINGS frames.
        let settings = h3::Settings::connect(&conn).await?;

        // Accept the CONNECT request but don't send a response yet.
        let connect = h3::Connecting::accept(&conn).await?;

        // Return the resulting request with a reference to the settings/connect streams.
        Ok(Self {
            conn,
            settings,
            connect,
        })
    }

    /// Accept the session, returning a 200 OK.
    pub async fn ok(self) -> Result<Connection, ServerError> {
        self.respond(ConnectResponse::OK).await
    }

    /// Accept the session with the given response.
    pub async fn respond(
        self,
        response: impl Into<ConnectResponse>,
    ) -> Result<Connection, ServerError> {
        let connect = self.connect.respond(response.into()).await?;
        Ok(Connection::new(self.conn, self.settings, connect))
    }

    /// Reject the session, returing your favorite HTTP status code.
    pub async fn reject(self, status: http::StatusCode) -> Result<(), ServerError> {
        self.connect.reject(status).await?;
        Ok(())
    }
}

impl core::ops::Deref for Request {
    type Target = h3::Connecting;

    fn deref(&self) -> &Self::Target {
        &self.connect
    }
}
