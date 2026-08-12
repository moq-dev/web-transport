use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::prelude::*;

/// A WebTransport error classified based on the source.
#[derive(Clone, Debug, thiserror::Error)]
pub enum Error {
    /// The session was closed, by us or by the peer.
    ///
    /// Carries the code and reason directly rather than a `WebTransportError`,
    /// because the browser will not build one of those with a session source: the
    /// `source` member of the constructor's dictionary is ignored and the result is
    /// always a stream error.
    #[error("webtransport session error: code={code} reason={reason:?}")]
    Session { code: u32, reason: String },

    // `WebTransportError` is a `DOMException`, so its message is the reason the peer
    // sent. Printing the JS object instead would bury that in a handle dump.
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
            Error::Session { code, .. } => Some(*code),
            Error::Stream(e) => stream_error_code(e),
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
                web_sys::WebTransportErrorSource::Session => Error::Session {
                    code: stream_error_code(&e).unwrap_or(0),
                    reason: e.message(),
                },
                _ => Error::Unknown(v),
            }
        } else {
            Error::Unknown(v)
        }
    }
}

/// The error a session close info describes, which is how a clean close reaches a
/// caller.
///
/// No JS object is built here. A session close code is a full `u32` — unlike a
/// stream code, which the browser clamps to a byte — and nothing would carry it.
pub(crate) fn session_error(info: &web_sys::WebTransportCloseInfo) -> Error {
    Error::Session {
        code: info.get_close_code().unwrap_or(0),
        reason: info.get_reason().unwrap_or_default(),
    }
}

/// The code carried by a `WebTransportError`, if it has one.
///
/// Read through `Reflect` rather than `web-sys`'s getter so a browser that widens
/// the field past a byte is reported faithfully rather than truncated into some
/// other code's meaning.
pub(crate) fn stream_error_code(err: &web_sys::WebTransportError) -> Option<u32> {
    let code = Reflect::get(err, &"streamErrorCode".into())
        .ok()?
        .as_f64()?;

    // Anything outside the range is not a code we could have been sent.
    (code >= 0.0 && code <= u32::MAX as f64).then_some(code as u32)
}

/// A `WebTransportError` carrying `code`, which is how a browser stream cancel or
/// abort names the STOP_SENDING or RESET_STREAM code it sends. Any other reason — a
/// plain string, say — reaches the peer as code 0.
///
/// Built by hand because `web-sys` binds the constructor as
/// `new WebTransportError(message, options)`, an older form that browsers reject
/// with a `TypeError`; it takes a single init dictionary. Falling back to that
/// binding would send every code as 0, since the thrown `TypeError` is not a
/// `WebTransportError` and the browser reads no code out of it.
///
/// Browsers clamp the code to a byte, so anything above 255 reaches the peer as
/// 255. That is deliberately not clamped here: the wire and the trait both carry a
/// `u32`, and a browser that implements the wider field should get the real code.
pub(crate) fn stream_error(code: u32) -> JsValue {
    let init = Object::new();
    let _ = Reflect::set(&init, &"streamErrorCode".into(), &JsValue::from(code));

    construct_error(&init).unwrap_or(JsValue::UNDEFINED)
}

/// `new WebTransportError(init)`, or `None` if the browser has no such constructor.
fn construct_error(init: &Object) -> Option<JsValue> {
    let ctor = Reflect::get(&js_sys::global(), &"WebTransportError".into()).ok()?;
    let ctor = ctor.dyn_into::<Function>().ok()?;

    Reflect::construct(&ctor, &Array::of1(init)).ok()
}
