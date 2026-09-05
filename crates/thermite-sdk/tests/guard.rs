//! What `Guard::drop` does on the way out of `main`.
//!
//! In a test binary of its own: the guard reports through the process-wide client, which is a
//! `OnceLock`, so exactly one test per binary may install one.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use thermite_sdk::{Options, Transport};

/// A transport that records sends *and* flushes on one timeline.
///
/// `TestTransport` records only sends, which cannot answer the question this file exists to ask —
/// whether the session was closed before or after the flush that was supposed to deliver it.
#[derive(Default)]
struct Timeline(Mutex<Vec<String>>);

impl Timeline {
    fn steps(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

impl Transport for Timeline {
    fn send(&self, envelope: Vec<u8>) {
        let body = String::from_utf8(envelope).expect("envelope is not UTF-8");
        let mut lines = body.lines().skip(1);

        let headers: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        let payload: Value = serde_json::from_str(lines.next().unwrap()).unwrap();

        let item = headers["type"].as_str().unwrap();
        let detail = match item {
            "session" => match payload["init"] == true {
                true => "init".to_string(),
                false => payload["status"].as_str().unwrap().to_string(),
            },
            _ => payload["level"].as_str().unwrap_or("?").to_string(),
        };

        self.0.lock().unwrap().push(format!("{item}:{detail}"));
    }

    fn flush(&self, _timeout: Duration) -> bool {
        self.0.lock().unwrap().push("flush".to_string());
        true
    }
}

/// The ordering is the whole point. `Guard::drop` closes the session and *then* flushes, so the
/// terminal update is in the queue the flush drains. Reversed, every process would exit with its
/// last session left open — and since thermite counts a session from its `init`, the crash-free
/// rate would be computed from starts that never recorded an outcome.
#[test]
fn dropping_the_guard_ends_the_session_before_flushing() {
    let timeline = Arc::new(Timeline::default());

    let mut options = Options::new("http://abc123@localhost:9000/42");
    // Release-scoped by definition: without one the session is never started at all.
    options.release = Some("1.4.2".to_string());
    options.attach_panic_hook = false;

    let guard = thermite_sdk::init_with(options, timeline.clone()).expect("invalid DSN");

    assert_eq!(
        timeline.steps(),
        vec!["session:init"],
        "init should have opened a session and sent nothing else"
    );

    thermite_sdk::capture_message("payment gateway unreachable", thermite_sdk::Level::Error);
    drop(guard);

    assert_eq!(
        timeline.steps(),
        vec!["session:init", "event:error", "session:exited", "flush"],
        "the session has to close before the flush that delivers it"
    );
}
