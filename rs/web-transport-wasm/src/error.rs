use wasm_bindgen::prelude::*;

/// A WebTransport error classified based on the source.
#[derive(Clone, Debug, thiserror::Error)]
pub enum Error {
    #[error("webtransport session error: {0:?}")]
    Session(web_sys::WebTransportError),

    #[error("webtransport stream error: {0:?}")]
    Stream(web_sys::WebTransportError),

    #[error("web streams error: {0:?}")]
    Streams(#[from] web_streams::Error),

    #[error("unknown error: {0:?}")]
    Unknown(JsValue),
}

impl Error {
    /// The error code used when closing the stream or session.
    pub fn code(&self) -> Option<u8> {
        match self {
            Error::Session(e) | Error::Stream(e) => e.stream_error_code(),
            _ => None,
        }
    }
}

impl From<JsValue> for Error {
    /// Convert a generic `JsValue` into a `WebTransportError` or `Error::Unknown`.
    fn from(v: JsValue) -> Self {
        if let Some(e) = v.dyn_ref::<web_sys::WebTransportError>().cloned() {
            match e.source() {
                web_sys::WebTransportErrorSource::Stream => Error::Stream(e),
                web_sys::WebTransportErrorSource::Session => Error::Session(e),
                _ => Error::Unknown(v),
            }
        } else {
            Error::Unknown(v)
        }
    }
}
