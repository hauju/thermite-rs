//! The browser transport: one `fetch` per envelope.
//!
//! No queue and no worker. The browser already owns the request queue, and `keepalive` hands the
//! request to the user agent to finish independently of the page — which matters because the
//! moment a front-end error is most likely to be the last thing that happened is the moment the
//! user navigates away from the page it happened on.
//!
//! This is where the crate earns its existence. Under `wasm32-unknown-unknown` the Sentry Rust SDK
//! has no threads to run its transport on and no backtraces to put in its events, so the machinery
//! it pays for in code size does nothing — while this file is a `fetch` call.

use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Request, RequestInit, Response};

use crate::transport::Transport;

/// Browsers cap a `keepalive` request body at 64 KiB across all in-flight keepalive requests.
///
/// An event that large is a payload problem rather than an error report, and losing it silently to
/// a rejected fetch would be worse than saying so.
const MAX_KEEPALIVE_BYTES: usize = 64 * 1024;

pub struct FetchTransport {
    url: String,
}

impl FetchTransport {
    /// `url` is `Dsn::ingest_url`, credentials included.
    pub fn new(url: String) -> Self {
        Self { url }
    }
}

impl Transport for FetchTransport {
    fn send(&self, envelope: Vec<u8>) {
        if envelope.len() > MAX_KEEPALIVE_BYTES {
            tracing::warn!(
                bytes = envelope.len(),
                "event exceeds the browser keepalive limit, dropping it"
            );
            return;
        }

        let url = self.url.clone();
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(error) = post(&url, envelope).await {
                tracing::warn!(?error, "could not deliver an envelope to thermite");
            }
        });
    }

    // No `flush` override. There is no queue of our own to drain: once `fetch` has the request,
    // `keepalive` is what carries it past the page's lifetime, and nothing we could wait on here
    // would make that more certain.
}

async fn post(url: &str, envelope: Vec<u8>) -> Result<(), JsValue> {
    let options = RequestInit::new();
    options.set_method("POST");

    // Not typed on `RequestInit` in web-sys 0.3, but `RequestInit` is a plain JS object and fetch
    // reads the property by name.
    js_sys::Reflect::set(&options, &"keepalive".into(), &JsValue::TRUE)?;

    let body = js_sys::Uint8Array::from(envelope.as_slice());
    options.set_body(body.as_ref());

    // Deliberately no `content-type`. It would be a non-safelisted value, which turns every send
    // into a CORS preflight plus the POST — and thermite ignores the header anyway, because
    // sentry-rust sends none either. Credentials ride on the URL's `?sentry_key=`, so no custom
    // request header is needed at all, which keeps this a simple request.
    let request = Request::new_with_str_and_init(url, &options)?;

    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window to fetch from"))?;
    let response: Response = JsFuture::from(window.fetch_with_request(&request))
        .await?
        .dyn_into()?;

    if !response.ok() {
        return Err(JsValue::from_str(&format!(
            "thermite responded {}",
            response.status()
        )));
    }
    Ok(())
}
