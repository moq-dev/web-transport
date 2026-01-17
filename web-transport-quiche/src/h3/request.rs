use url::Url;

use crate::{ez, h3, proto::ConnectResponse, Connection, ServerError};

/// A mostly complete WebTransport handshake, just awaiting the server's decision on whether to accept or reject the session based on the URL.
pub struct Request {
    conn: ez::Connection,
    settings: h3::Settings,
    connect: h3::Connect,
}

impl Request {
    /// Accept a new WebTransport session from a client.
    pub async fn accept(conn: ez::Connection) -> Result<Self, ServerError> {
        // Perform the H3 handshake by sending/reciving SETTINGS frames.
        let settings = h3::Settings::connect(&conn).await?;

        // Accept the CONNECT request but don't send a response yet.
        let connect = h3::Connect::accept(&conn).await?;

        // Return the resulting request with a reference to the settings/connect streams.
        Ok(Self {
            conn,
            settings,
            connect,
        })
    }

    /// Returns the URL provided by the client.
    pub fn url(&self) -> &Url {
        self.connect.url()
    }

    /// Accept the session, returning a 200 OK.
    pub async fn respond(
        mut self,
        response: impl Into<ConnectResponse>,
    ) -> Result<Connection, ServerError> {
        self.connect.respond(response.into()).await?;
        Ok(Connection::new(self.conn, self.settings, self.connect))
    }

    /// Reject the session, returing your favorite HTTP status code.
    pub async fn close(mut self, status: http::StatusCode) -> Result<(), ServerError> {
        self.connect.respond(status).await?;
        Ok(())
    }
}
