//! Where a serialized envelope goes.
//!
//! One trait with two shapes behind it: a background HTTP sender on native targets, and a
//! `fetch` call on wasm. Both are fire-and-forget — reporting an error must never fail the code
//! path that raised it, so `send` returns nothing and cannot be awaited.
//!
//! `flush` exists for the two moments where that is not enough: process shutdown, and tests. It is
//! the only blocking call in the crate.

use std::sync::Mutex;
use std::time::Duration;

#[cfg(all(target_arch = "wasm32", feature = "web"))]
mod fetch;
#[cfg(feature = "native")]
mod http;

#[cfg(all(target_arch = "wasm32", feature = "web"))]
pub use fetch::FetchTransport;
#[cfg(feature = "native")]
pub use http::HttpTransport;

pub trait Transport: Send + Sync {
    /// Hands an envelope over for delivery.
    ///
    /// Errors are the transport's own problem. A dropped envelope is a lost event, which is worth
    /// logging and never worth propagating: the caller is already handling a failure.
    fn send(&self, envelope: Vec<u8>);

    /// Blocks until queued envelopes are sent, or `timeout` expires. Returns whether the queue
    /// drained. The default suits transports that send synchronously and have nothing to wait for.
    fn flush(&self, timeout: Duration) -> bool {
        let _ = timeout;
        true
    }
}

/// A transport that records instead of sending.
///
/// Public because the same question — "did my instrumentation actually produce the event I think
/// it did?" — is one the applications reporting into thermite need to answer in their own tests.
#[derive(Debug, Default)]
pub struct TestTransport {
    sent: Mutex<Vec<Vec<u8>>>,
}

impl TestTransport {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every envelope handed over so far, oldest first.
    pub fn envelopes(&self) -> Vec<Vec<u8>> {
        self.sent.lock().expect("test transport poisoned").clone()
    }

    /// The envelopes as UTF-8, for assertions against the wire format.
    pub fn bodies(&self) -> Vec<String> {
        self.envelopes()
            .into_iter()
            .map(|body| String::from_utf8(body).expect("envelope is not UTF-8"))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.sent.lock().expect("test transport poisoned").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Transport for TestTransport {
    fn send(&self, envelope: Vec<u8>) {
        self.sent
            .lock()
            .expect("test transport poisoned")
            .push(envelope);
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use uuid::Uuid;

    use super::*;
    use crate::dsn::Dsn;
    use crate::envelope::event_envelope;
    use crate::event::{Event, Level};

    #[test]
    fn records_envelopes_in_order() {
        let transport = TestTransport::new();
        assert!(transport.is_empty());

        transport.send(b"first".to_vec());
        transport.send(b"second".to_vec());

        assert_eq!(transport.len(), 2);
        assert_eq!(transport.bodies(), vec!["first", "second"]);
    }

    /// The end of step one: an event goes in, wire-format bytes come out the other side of a
    /// transport, with nothing in between that needs a network.
    #[test]
    fn carries_a_real_envelope_end_to_end() {
        let dsn = Dsn::parse("http://abc123@localhost:9000/42").unwrap();
        let mut event = Event::message("payment gateway unreachable", Level::Error);
        event.event_id = Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap();
        event.timestamp = Utc.with_ymd_and_hms(2026, 8, 29, 10, 0, 0).unwrap();

        let transport = TestTransport::new();
        transport.send(event_envelope(&event, &dsn).unwrap());

        let body = transport.bodies().remove(0);
        let mut lines = body.lines();

        assert!(
            lines
                .next()
                .unwrap()
                .contains("00000000000040008000000000000001")
        );
        assert_eq!(lines.next().unwrap(), r#"{"type":"event"}"#);
        assert!(
            lines
                .next()
                .unwrap()
                .contains("payment gateway unreachable")
        );
        assert!(lines.next().is_none());
    }

    #[test]
    fn flushing_a_synchronous_transport_always_succeeds() {
        assert!(TestTransport::new().flush(Duration::from_secs(1)));
    }
}
