use js_sys::Uint8Array;
use url::Url;
use wasm_bindgen_futures::JsFuture;
use web_sys::{WebTransport, WebTransportHash, WebTransportOptions};

use crate::{Error, Session};

pub use web_sys::WebTransportCongestionControl as CongestionControl;

/// See [`WebTransportOptions`].
#[derive(Debug, Default)]
pub struct ClientBuilder {
    options: WebTransportOptions,
}

impl ClientBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Determine if the client/server is allowed to pool connections.
    /// (Hint) Don't set it to true.
    pub fn with_pooling(self, val: bool) -> Self {
        self.options.set_allow_pooling(val);
        self
    }

    /// `true` if QUIC is required, `false` if TCP is a valid fallback.
    pub fn with_unreliable(self, val: bool) -> Self {
        self.options.set_require_unreliable(val);
        self
    }

    /// Hint at the required congestion control algorithm
    pub fn with_congestion_control(self, control: CongestionControl) -> Self {
        self.options.set_congestion_control(control);
        self
    }

    /// Supply sha256 hashes for accepted certificates, instead of using a root CA
    pub fn with_server_certificate_hashes(self, hashes: Vec<Vec<u8>>) -> Client {
        let hashes: Vec<WebTransportHash> = hashes
            .into_iter()
            .map(|hash| {
                let entry = WebTransportHash::new();
                entry.set_algorithm("sha-256");
                // Workaround as .set_value_u8_slice does not work properly.
                entry.set_value_u8_array(&Uint8Array::new_from_slice(hash.as_slice()));
                entry
            })
            .collect();

        self.options
            .set_server_certificate_hashes(hashes.as_slice());
        Client {
            options: self.options,
        }
    }

    pub fn with_system_roots(self) -> Client {
        Client {
            options: self.options,
        }
    }
}

/// Build a client with the given URL and options.
///
/// See [`WebTransportOptions`].
#[derive(Clone, Debug, Default)]
pub struct Client {
    options: WebTransportOptions,
}

impl Client {
    /// Connect once the builder is configured.
    pub async fn connect(&self, url: Url) -> Result<Session, Error> {
        let inner = WebTransport::new_with_options(url.as_str(), &self.options)?;
        JsFuture::from(inner.ready()).await?;

        Ok(Session::new(inner, url))
    }
}
