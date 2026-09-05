//! The panic path, end to end.
//!
//! In a test binary of its own on purpose. It installs the process-wide client and replaces the
//! process-wide panic hook, and both are things exactly one test may do — sharing a binary with
//! others would mean racing them for the hook and swallowing their failure output.

use std::sync::Arc;

use serde_json::Value;
use thermite_sdk::{Options, TestTransport};

/// The headline path: a real panic, through the real hook, into a real envelope.
///
/// The unit tests cover the pieces — reading the payload, building the location frame, trimming
/// the stack. Only this covers `install` actually wiring them to the client.
#[test]
fn a_panic_is_reported_as_a_fatal_exception_with_a_stack() {
    // Chained onto by our hook, so the deliberate panic below prints nothing and does not read as
    // a failure in the test output.
    std::panic::set_hook(Box::new(|_| {}));

    let recorder = Arc::new(TestTransport::new());
    // No release, so auto session tracking is a no-op and the only envelope is the panic itself.
    let _guard = thermite_sdk::init_with(
        Options::new("http://abc123@localhost:9000/42"),
        recorder.clone(),
    )
    .expect("invalid DSN");

    let panicked = std::panic::catch_unwind(|| panic!("payment gateway unreachable"));
    assert!(panicked.is_err(), "the closure was supposed to panic");

    let bodies = recorder.bodies();
    assert_eq!(bodies.len(), 1, "exactly one envelope: {bodies:?}");

    let event: Value = serde_json::from_str(bodies[0].lines().nth(2).unwrap()).unwrap();
    let exception = &event["exception"]["values"][0];

    assert_eq!(event["level"], Value::from("fatal"));
    assert_eq!(exception["type"], Value::from("panic"));
    assert_eq!(
        exception["value"],
        Value::from("payment gateway unreachable")
    );
    assert_eq!(exception["mechanism"]["handled"], Value::from(false));

    let frames = exception["stacktrace"]["frames"].as_array().unwrap();

    // The location frame is appended innermost-last, and it is the only one that still carries a
    // file and line in a release build with no debug info.
    let innermost = frames.last().unwrap();
    assert!(
        innermost["filename"]
            .as_str()
            .unwrap()
            .ends_with("panic.rs"),
        "the innermost frame should be the panic site: {innermost:?}"
    );

    // A walked stack, not just that one frame — this is what gives the issue a culprit.
    assert!(
        frames.len() > 1,
        "expected the panicking stack, got only the location frame"
    );
    assert!(
        frames
            .iter()
            .any(|frame| frame["in_app"] == true && frame["function"].is_string()),
        "no frame arrived marked in_app with a resolved function name"
    );

    // The reporter's own frames are trimmed, or every panic's culprit would be `capture_event`.
    assert!(
        !frames.iter().any(|frame| frame["function"]
            .as_str()
            .is_some_and(|function| function.starts_with("thermite_sdk"))),
        "the SDK's own frames should have been trimmed: {frames:?}"
    );
}
