//! Turning a panic into an event.
//!
//! The hook chains rather than replaces: whatever was installed before still runs, so the default
//! stderr message an operator expects to see is not swallowed by installing this.
//!
//! Under the native backend a panic carries the walked stack plus the panic site. Without it the
//! panic site is all there is: it renders as `file:line` on the issue page, but it has no
//! `function`, and thermite's `crash_location` skips frames without one — so a panic reported from
//! wasm groups and reads fine while its culprit stays empty.

use std::any::Any;
use std::panic::{Location, PanicHookInfo};

use crate::event::{Event, Exception, Frame, Level, Mechanism, Stacktrace};

/// Installs a panic hook that reports through the initialized client.
///
/// Called by `init` unless `Options::attach_panic_hook` is cleared.
pub fn install() {
    let previous = std::panic::take_hook();

    std::panic::set_hook(Box::new(move |info| {
        crate::capture_event(event_from_panic(info));

        // After the capture, which has already counted this run as errored. Thermite reads
        // `errored` only off an `exited` session, so a crashed one is not double-counted — and
        // the terminal update goes out when the guard drops on the way out of `main`.
        #[cfg(feature = "sessions")]
        crate::mark_session_crashed();

        previous(info);
    }));
}

/// The event a panic becomes: one unhandled exception typed `panic`.
pub fn event_from_panic(info: &PanicHookInfo<'_>) -> Event {
    let mut exception = Exception::new("panic", message_from_payload(info.payload()));

    exception.mechanism = Some(Mechanism {
        r#type: "panic".to_string(),
        handled: Some(false),
        synthetic: None,
    });
    exception.stacktrace = stacktrace(info);

    let mut event = Event::exception(exception);
    event.level = Level::Fatal;
    event
}

/// The panicking stack, with the panic site appended as the innermost frame.
///
/// The hook runs on the panicking thread before it unwinds, so this walks the stack that panicked
/// rather than the hook's own. The location frame goes on the end because it is the only one that
/// still carries a file and line in a release build stripped of debug info.
#[cfg(feature = "native")]
fn stacktrace(info: &PanicHookInfo<'_>) -> Option<Stacktrace> {
    let mut stack = crate::stacktrace::capture();
    stack
        .frames
        .extend(info.location().map(frame_from_location));

    (!stack.frames.is_empty()).then_some(stack)
}

#[cfg(not(feature = "native"))]
fn stacktrace(info: &PanicHookInfo<'_>) -> Option<Stacktrace> {
    info.location().map(|location| Stacktrace {
        frames: vec![frame_from_location(location)],
    })
}

/// The panic message.
///
/// A payload is `&str` for a literal `panic!("…")` and `String` once anything is formatted into
/// it. Anything else came from `panic_any` and has no text to read.
fn message_from_payload(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "panic with a non-string payload".to_string()
}

/// Where the panic happened, as the one frame we can name without unwinding.
fn frame_from_location(location: &Location<'_>) -> Frame {
    Frame {
        filename: Some(location.file().to_string()),
        lineno: Some(location.line()),
        colno: Some(location.column()),
        // The panicking file is the application's own, near enough: a panic raised inside a
        // dependency still points at the line that tripped it.
        in_app: true,
        ..Frame::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_literal_panic_message() {
        assert_eq!(message_from_payload(&"boom"), "boom");
    }

    #[test]
    fn reads_a_formatted_panic_message() {
        assert_eq!(message_from_payload(&"boom: 42".to_string()), "boom: 42");
    }

    /// `panic_any(0u8)` carries no text. Reporting *something* beats reporting nothing, since the
    /// location frame is still useful.
    #[test]
    fn falls_back_when_the_payload_is_not_a_string() {
        assert_eq!(
            message_from_payload(&0u8),
            "panic with a non-string payload"
        );
    }

    /// `Location` cannot be constructed, but `caller()` hands out a real one.
    #[test]
    fn a_location_becomes_a_frame_with_file_and_line() {
        let frame = frame_from_location(Location::caller());

        assert!(frame.filename.unwrap().ends_with("panic.rs"));
        assert!(frame.lineno.is_some());
        assert!(frame.in_app);
        assert!(
            frame.function.is_none(),
            "no function name is available without unwinding"
        );
    }
}
