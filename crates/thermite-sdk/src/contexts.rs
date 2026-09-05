//! What the process is running on.
//!
//! Read once at `init` and stamped onto every event. Thermite does not index contexts, but the
//! issue detail page flattens them to dotted keys and the `get_issue` MCP tool promises them, so
//! this is the difference between an agent knowing which platform crashed and guessing.
//!
//! Deliberately thin. Sentry's own `sentry-contexts` is a whole crate because it reads OS release
//! strings out of `/etc/os-release`, `uname` and the Windows registry; none of that survives the
//! wasm build, and none of it has ever been the fact that explained a bug here.

use std::collections::BTreeMap;

use serde_json::{Value, json};

/// The browser: everything useful is on `window`, and there is no OS to ask.
///
/// The user agent goes over verbatim rather than parsed into name and version. Parsing it is a
/// well-known losing game, and thermite renders `browser.user_agent` as readably as it would
/// render a half-right guess at the browser's name.
#[cfg(all(target_arch = "wasm32", feature = "web"))]
pub fn describe() -> BTreeMap<String, Value> {
    let mut contexts = BTreeMap::new();

    let Some(window) = web_sys::window() else {
        return contexts;
    };

    if let Ok(user_agent) = window.navigator().user_agent() {
        contexts.insert("browser".to_string(), json!({ "user_agent": user_agent }));
    }
    if let Ok(url) = window.location().href() {
        // Which page was open. The nearest thing a browser has to `server_name`, and the first
        // question asked about any front-end error.
        contexts.insert("page".to_string(), json!({ "url": url }));
    }

    contexts
}

/// Everything else: the three facts `std` will tell us without asking the operating system.
#[cfg(not(all(target_arch = "wasm32", feature = "web")))]
pub fn describe() -> BTreeMap<String, Value> {
    BTreeMap::from([
        ("os".to_string(), json!({ "name": std::env::consts::OS })),
        (
            "device".to_string(),
            json!({ "arch": std::env::consts::ARCH }),
        ),
        // No version: the compiler's is not readable at runtime, and reporting this crate's MSRV
        // as though it were the toolchain would be worse than saying nothing.
        ("runtime".to_string(), json!({ "name": "rust" })),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Names the host without shelling out or reading a file, which is the whole point: `describe`
    /// runs inside `init`, on the caller's thread.
    #[test]
    fn describes_the_host_platform() {
        let contexts = describe();

        assert_eq!(contexts["os"]["name"], Value::from(std::env::consts::OS));
        assert_eq!(
            contexts["device"]["arch"],
            Value::from(std::env::consts::ARCH)
        );
        assert_eq!(contexts["runtime"]["name"], Value::from("rust"));
    }
}
