use js_sys::Reflect;
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
    pub fn code(&self) -> Option<u32> {
        match self {
            Error::Session(e) | Error::Stream(e) => stream_error_code(e),
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

// `web-sys` types `streamErrorCode` as a byte, from a WebIDL draft that has since
// widened it to `unsigned long`. Its setter would send any code above 255 as none,
// and its getter truncates one into a different code's meaning, so both directions
// go around the binding until `web-sys` catches up.
//
// <https://www.w3.org/TR/webtransport/#dictdef-webtransporterroroptions>

/// The code carried by a `WebTransportError`, if it has one.
pub(crate) fn stream_error_code(err: &web_sys::WebTransportError) -> Option<u32> {
    let code = Reflect::get(err, &"streamErrorCode".into())
        .ok()?
        .as_f64()?;

    // Anything outside the range is not a code we could have been sent.
    (code >= 0.0 && code <= u32::MAX as f64).then_some(code as u32)
}

/// A `WebTransportError` carrying `code`, which is how a browser stream cancel or
/// abort names the STOP_SENDING or RESET_STREAM code it sends.
///
/// Any other reason — a plain string, say — reaches the peer as code 0.
pub(crate) fn stream_error(code: u32) -> JsValue {
    let options = web_sys::WebTransportErrorOptions::new();
    options.set_source(web_sys::WebTransportErrorSource::Stream);
    let _ = Reflect::set(&options, &"streamErrorCode".into(), &JsValue::from(code));

    match web_sys::WebTransportError::new_with_message_and_options("", &options) {
        Ok(err) => err.into(),
        Err(err) => err,
    }
}

/// The error a session close info describes, which is how a clean close reaches a
/// caller.
pub(crate) fn session_error(info: &web_sys::WebTransportCloseInfo) -> Error {
    let reason = info.get_reason().unwrap_or_default();

    let options = web_sys::WebTransportErrorOptions::new();
    options.set_source(web_sys::WebTransportErrorSource::Session);

    let code = info.get_close_code().unwrap_or(0);
    let _ = Reflect::set(&options, &"streamErrorCode".into(), &JsValue::from(code));

    match web_sys::WebTransportError::new_with_message_and_options(&reason, &options) {
        Ok(err) => Error::Session(err),
        Err(err) => Error::from(err),
    }
}
