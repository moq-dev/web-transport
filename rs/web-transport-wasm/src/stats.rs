use js_sys::{Function, Reflect};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

use crate::Error;

/// Feature detection is per stream: older browsers expose ordinary Streams.
pub(crate) struct Collector {
    stream: JsValue,
    get_stats: Option<Function>,
}

impl Collector {
    pub(crate) fn new(stream: &JsValue) -> Result<Self, Error> {
        let method = Reflect::get(stream, &"getStats".into())?;
        let get_stats = if method.is_undefined() {
            None
        } else {
            Some(
                method
                    .dyn_into()
                    .map_err(|_| invalid("getStats is not callable"))?,
            )
        };
        Ok(Self {
            stream: stream.clone(),
            get_stats,
        })
    }

    pub(crate) async fn collect(&self) -> Result<JsValue, Error> {
        let Some(method) = &self.get_stats else {
            return Ok(JsValue::UNDEFINED);
        };
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

pub(crate) fn metric(stats: &JsValue, name: &str) -> Result<Option<u64>, Error> {
    if stats.is_undefined() {
        return Ok(None);
    }
    let value = Reflect::get(stats, &name.into())?;
    if value.is_undefined() {
        return Ok(None);
    }
    // WebIDL uses JS Numbers here. Do not silently round an unsafe integer or
    // cast malformed values to zero; either would invent measured progress.
    let number = value
        .as_f64()
        .filter(|n| n.is_finite() && *n >= 0.0 && *n <= 9_007_199_254_740_991.0 && n.fract() == 0.0)
        .ok_or_else(|| invalid("stream statistic is not a nonnegative safe integer"))?;
    Ok(Some(number as u64))
}

fn invalid(message: &str) -> Error {
    JsValue::from(js_sys::TypeError::new(message)).into()
}
