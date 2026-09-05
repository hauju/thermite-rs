//! Turning the current call stack into frames.
//!
//! Native only. Under `wasm32-unknown-unknown` there is no stack to walk and no symbol table to
//! resolve it against, which is true of the Sentry SDK there too — it is one of the reasons this
//! crate exists rather than the larger one.
//!
//! Note what the stack actually points at when it comes from [`crate::capture_error`]: where the
//! error was *reported*, not where it was created. Rust errors are values, and a value carries no
//! record of its own construction. sentry-rust has the same limitation for the same reason.

use crate::event::{Frame, Stacktrace};

/// Function-name prefixes that are never the application's own code.
///
/// A heuristic, and the same shape as sentry-backtrace's: nothing in a symbol name says "this is a
/// dependency", so the runtime and the frameworks that always sit between `main` and the failure
/// are listed instead. Being wrong here moves the culprit one frame, it does not lose an event.
const NOT_IN_APP: &[&str] = &[
    "std::",
    "core::",
    "alloc::",
    "<std::",
    "<core::",
    "<alloc::",
    "backtrace::",
    "thermite_sdk::",
    "tokio::",
    "futures_core::",
    "futures_util::",
    "hyper::",
    "axum::",
    "tower::",
    "__rust_",
    "rust_begin_unwind",
];

/// The stack at the call site, outermost-first.
pub fn capture() -> Stacktrace {
    let backtrace = backtrace::Backtrace::new();

    // `backtrace` hands frames back innermost-first, which is the order the trimming below needs.
    let mut frames: Vec<Frame> = backtrace
        .frames()
        .iter()
        .flat_map(backtrace::BacktraceFrame::symbols)
        .map(frame_from_symbol)
        .collect();

    trim_own_frames(&mut frames);

    // Sentry orders frames outermost-first; thermite walks them in reverse to find the crash site.
    frames.reverse();
    Stacktrace { frames }
}

fn frame_from_symbol(symbol: &backtrace::BacktraceSymbol) -> Frame {
    // `{:#}` is the demangled form without the trailing hash, which is what makes two builds of
    // one function group together.
    let function = symbol.name().map(|name| format!("{name:#}"));

    Frame {
        in_app: function.as_deref().is_some_and(is_in_app),
        function,
        filename: symbol.filename().map(|path| path.display().to_string()),
        lineno: symbol.lineno(),
        colno: symbol.colno(),
        ..Frame::default()
    }
}

/// Drops this crate's own frames from the innermost end.
///
/// Without it the innermost in-app frame is always `thermite_sdk::capture_error`, and since that
/// is what becomes the issue's culprit, every issue would be attributed to the reporter rather
/// than to the code that failed.
///
/// Takes the *outermost* of our frames as the cut, not the innermost: `capture` is called through
/// `capture_error`, so stopping at the first match would leave our own entry point in the stack.
fn trim_own_frames(frames: &mut Vec<Frame>) {
    let own = frames.iter().rposition(|frame| {
        frame
            .function
            .as_deref()
            .is_some_and(|function| function.starts_with("thermite_sdk"))
    });

    if let Some(index) = own {
        frames.drain(..=index);
    }
}

fn is_in_app(function: &str) -> bool {
    !NOT_IN_APP.iter().any(|prefix| function.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(function: &str) -> Frame {
        Frame {
            function: Some(function.to_string()),
            ..Frame::default()
        }
    }

    fn functions(frames: &[Frame]) -> Vec<&str> {
        frames
            .iter()
            .filter_map(|frame| frame.function.as_deref())
            .collect()
    }

    #[test]
    fn application_code_is_in_app_and_the_runtime_is_not() {
        assert!(is_in_app("checkout::billing::charge"));
        assert!(is_in_app("main"));

        assert!(!is_in_app("std::panicking::begin_panic"));
        assert!(!is_in_app("core::ops::function::FnOnce::call_once"));
        assert!(!is_in_app("<alloc::vec::Vec<T> as core::fmt::Debug>::fmt"));
        assert!(!is_in_app("tokio::runtime::park::Parker::park"));
        assert!(!is_in_app("thermite_sdk::capture_error"));
    }

    /// The cut is the *outermost* of our frames. Stopping at the innermost would leave
    /// `capture_error` in the stack, and it would then be the culprit of every issue.
    #[test]
    fn trimming_cuts_past_the_outermost_of_our_own_frames() {
        let mut frames = vec![
            named("backtrace::backtrace::trace"),
            named("thermite_sdk::stacktrace::capture"),
            named("thermite_sdk::capture_error"),
            named("checkout::billing::charge"),
            named("main"),
        ];

        trim_own_frames(&mut frames);

        assert_eq!(
            functions(&frames),
            vec!["checkout::billing::charge", "main"]
        );
    }

    /// A stack with none of our frames in it — a panic in a thread we never touched — is kept
    /// whole rather than emptied.
    #[test]
    fn trimming_leaves_a_stack_that_never_entered_this_crate() {
        let mut frames = vec![named("checkout::billing::charge"), named("main")];

        trim_own_frames(&mut frames);

        assert_eq!(
            functions(&frames),
            vec!["checkout::billing::charge", "main"]
        );
    }

    /// The live check that symbolication works at all: without debug info this returns frames with
    /// no names, and every culprit would silently be empty.
    #[test]
    fn a_captured_stack_resolves_function_names() {
        let stack = capture();

        assert!(!stack.frames.is_empty(), "no frames were captured");
        assert!(
            stack.frames.iter().any(|frame| frame.function.is_some()),
            "no frame resolved to a symbol"
        );
        assert!(
            !functions(&stack.frames)
                .iter()
                .any(|function| function.starts_with("thermite_sdk::stacktrace")),
            "our own capture frames should have been trimmed"
        );
    }
}
