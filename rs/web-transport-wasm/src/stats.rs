use js_sys::{Function, Reflect};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

use crate::Error;

/// Feature detection is per stream: older browsers expose ordinary Streams.
pub(crate) struct Collector {
    stream: JsValue,
}

impl Collector {
    pub(crate) fn new(stream: &JsValue) -> Self {
        Self {
            stream: stream.clone(),
        }
    }

    /// Fetch a fresh `getStats()` dictionary, or `undefined` if the browser lacks it.
    pub(crate) async fn collect(&self) -> Result<JsValue, Error> {
        // Optional statistics must not add a failure mode to opening or accepting
        // a stream. Surface broken collectors only when collection is requested.
        let method = Reflect::get(&self.stream, &"getStats".into())?;
        if method.is_undefined() {
            return Ok(JsValue::UNDEFINED);
        }
        let method = method
            .dyn_into::<Function>()
            .map_err(|_| invalid("getStats is not callable"))?;
        let result = method.call0(&self.stream)?;
        let promise = result
            .dyn_into::<js_sys::Promise>()
            .map_err(|_| invalid("getStats did not return a Promise"))?;
        let stats = JsFuture::from(promise).await?;
        if !stats.is_object() || stats.is_null() {
            return Err(invalid("getStats did not return a dictionary"));
        }
        Ok(stats)
    }
}

/// Read one W3C counter: `None` when the dictionary or member is absent, an error when malformed.
pub(crate) fn metric(stats: &JsValue, name: &str) -> Result<Option<u64>, Error> {
    if stats.is_undefined() {
        return Ok(None);
    }
    let value = Reflect::get(stats, &name.into())?;
    if value.is_undefined() {
        return Ok(None);
    }
    // W3C fields are non-nullable unsigned long long values, omitted when
    // unavailable. Do not round unsafe JS Numbers or cast malformed values to zero.
    let number = value
        .as_f64()
        .filter(|n| n.is_finite() && *n >= 0.0 && *n <= 9_007_199_254_740_991.0 && n.fract() == 0.0)
        .ok_or_else(|| invalid("stream statistic is not a nonnegative safe integer"))?;
    Ok(Some(number as u64))
}

fn invalid(message: &str) -> Error {
    JsValue::from(js_sys::TypeError::new(message)).into()
}
