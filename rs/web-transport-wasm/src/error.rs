use wasm_bindgen::prelude::*;

/// A WebTransport error classified based on the source.
#[derive(Clone, Debug, thiserror::Error)]
pub enum Error {
    // `WebTransportError` is a `DOMException`, so its message is the reason the peer
    // sent. Printing the JS object instead would bury that in a handle dump.
    #[error("webtransport session error: {}", .0.message())]
    Session(web_sys::WebTransportError),

    #[error("webtransport stream error: {}", .0.message())]
    Stream(web_sys::WebTransportError),

    /// The stream was closed locally, by [`crate::SendStream::finish`] or
    /// [`crate::SendStream::reset`], so it can accept no more writes.
    #[error("stream is closed")]
    Closed,

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

/// A `WebTransportError` carrying `code`, which is how a browser stream cancel or
/// abort names the STOP_SENDING or RESET_STREAM code it sends.
///
/// Any other reason — a plain string, say — reaches the peer as code 0. The browser
/// API carries the code as a byte, so a larger one is sent as 0 rather than
/// truncated into some other code's meaning.
pub(crate) fn stream_error(code: u32) -> JsValue {
    let options = web_sys::WebTransportErrorOptions::new();
    options.set_source(web_sys::WebTransportErrorSource::Stream);
    options.set_stream_error_code(u8::try_from(code).ok());

    match web_sys::WebTransportError::new_with_message_and_options("", &options) {
        Ok(err) => err.into(),
        Err(err) => err,
    }
}
