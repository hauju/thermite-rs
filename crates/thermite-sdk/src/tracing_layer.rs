//! A `tracing` layer that reports what the application logs.
//!
//! This is the integration that matters for a Rust server: most failures are already logged, and
//! without it every one of them has to be reported a second time by hand.
//!
//! `ERROR` becomes an event. Everything else becomes a breadcrumb, so the twenty lines leading up
//! to a failure ride along with it — which is most of what makes a report diagnosable and is
//! otherwise thrown away.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::Arc;

use serde_json::Value;
use tracing::field::{Field, Visit};
use tracing::{Event as Record, Level as RecordLevel, Metadata, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

use crate::event::{Breadcrumb, Event, Level};
use crate::{Client, Scope};

/// Records from this crate are never reported.
///
/// The transport logs a failed delivery with `tracing::error!`. Without this, that log becomes an
/// event, which is sent, which fails, which logs — a loop with no bound that spins fastest exactly
/// when thermite is unreachable, which is the worst possible moment to be generating traffic.
const OWN_TARGET: &str = "thermite_sdk";

/// Reports `tracing` records to thermite.
pub struct ThermiteLayer {
    /// `None` means the process-wide client that [`crate::init`] installs.
    client: Option<Arc<Client>>,
}

impl ThermiteLayer {
    /// Reports through the client `init` installed.
    pub fn new() -> Self {
        Self { client: None }
    }

    /// Reports through a specific client, for a process holding more than one.
    pub fn with_client(client: Arc<Client>) -> Self {
        Self {
            client: Some(client),
        }
    }

    fn capture(&self, event: Event) {
        match &self.client {
            Some(client) => {
                client.capture(event);
            }
            None => {
                crate::capture_event(event);
            }
        }
    }

    fn breadcrumb(&self, breadcrumb: Breadcrumb) {
        let record = |scope: &mut Scope| scope.add_breadcrumb(breadcrumb);
        match &self.client {
            Some(client) => client.configure_scope(record),
            None => crate::configure_scope(record),
        }
    }
}

impl Default for ThermiteLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Subscriber> Layer<S> for ThermiteLayer {
    fn on_event(&self, record: &Record<'_>, _context: Context<'_, S>) {
        let metadata = record.metadata();
        if metadata.target().starts_with(OWN_TARGET) {
            return;
        }

        let mut fields = Fields::default();
        record.record(&mut fields);

        if *metadata.level() == RecordLevel::ERROR {
            self.capture(event_from(metadata, fields));
        } else {
            self.breadcrumb(breadcrumb_from(metadata, fields));
        }
    }
}

fn event_from(metadata: &Metadata<'_>, fields: Fields) -> Event {
    let mut event = Event::message(fields.message(metadata), Level::Error);

    // The module that logged it. There are no stack frames behind a log line, so this is the only
    // thing on the event that says where it came from.
    event.logger = Some(metadata.target().to_string());
    event.extra = fields.extra;
    event
}

fn breadcrumb_from(metadata: &Metadata<'_>, fields: Fields) -> Breadcrumb {
    let mut breadcrumb = Breadcrumb::new(fields.message(metadata));

    breadcrumb.category = Some(metadata.target().to_string());
    breadcrumb.level = match *metadata.level() {
        RecordLevel::ERROR => Level::Error,
        RecordLevel::WARN => Level::Warning,
        RecordLevel::INFO => Level::Info,
        RecordLevel::DEBUG | RecordLevel::TRACE => Level::Debug,
    };
    breadcrumb.data = fields.data();
    breadcrumb
}

/// A record's fields, with the formatted message pulled out of them.
#[derive(Debug, Default)]
struct Fields {
    message: Option<String>,
    extra: BTreeMap<String, Value>,
}

impl Fields {
    /// The message, falling back to the record's name — which for a bare `tracing::error!(code =
    /// 5)` with no message is `event src/lib.rs:12` and still identifies the line.
    fn message(&self, metadata: &Metadata<'_>) -> String {
        self.message
            .clone()
            .unwrap_or_else(|| metadata.name().to_string())
    }

    /// The fields, capped.
    ///
    /// Breadcrumbs ride on every later event, so an unbounded field set on one log line is paid
    /// for by every report until the process exits.
    fn data(self) -> BTreeMap<String, Value> {
        const MAX_BREADCRUMB_FIELDS: usize = 10;

        self.extra.into_iter().take(MAX_BREADCRUMB_FIELDS).collect()
    }

    fn insert(&mut self, field: &Field, value: Value) {
        // `message` is what the record *says*, not one of its fields.
        if field.name() == "message"
            && let Value::String(message) = value
        {
            self.message = Some(message);
            return;
        }
        self.extra.insert(field.name().to_string(), value);
    }
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.insert(field, Value::from(value));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.insert(field, Value::from(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.insert(field, Value::from(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.insert(field, Value::from(value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.insert(field, Value::from(value));
    }

    /// The catch-all, and the one that carries `message`: `tracing` records a formatted message as
    /// `fmt::Arguments`, whose `Debug` is its `Display`, so this yields the text unquoted.
    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        self.insert(field, Value::from(format!("{value:?}")));
    }
}

#[cfg(test)]
mod tests {
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;
    use crate::{Options, TestTransport};

    /// A target outside this crate.
    ///
    /// Records emitted from a test module here would carry `thermite_sdk::…` as their target and
    /// be dropped by the loop-breaker — which is correct, and is why every test below has to log
    /// as the application would.
    const APP: &str = "checkout::billing";

    /// Runs `emit` with the layer installed, and returns the events it reported.
    fn reported(emit: impl FnOnce()) -> Vec<Value> {
        let recorder = Arc::new(TestTransport::new());
        let client = Arc::new(
            Client::with_transport(
                Options::new("http://abc123@localhost:9000/42"),
                recorder.clone(),
            )
            .unwrap(),
        );

        let subscriber =
            tracing_subscriber::registry().with(ThermiteLayer::with_client(client.clone()));
        tracing::subscriber::with_default(subscriber, emit);

        recorder
            .bodies()
            .iter()
            .map(|body| serde_json::from_str(body.lines().nth(2).unwrap()).unwrap())
            .collect()
    }

    #[test]
    fn an_error_log_becomes_an_event() {
        let events = reported(|| tracing::error!(target: APP, "payment gateway unreachable"));

        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0]["logentry"]["message"],
            Value::from("payment gateway unreachable")
        );
        assert_eq!(events[0]["level"], Value::from("error"));
        assert_eq!(events[0]["logger"], Value::from(APP));
    }

    #[test]
    fn record_fields_ride_along_as_extra() {
        let events = reported(
            || tracing::error!(target: APP, attempt = 3, route = "/charge", "charge failed"),
        );

        assert_eq!(events[0]["extra"]["attempt"], Value::from(3));
        assert_eq!(events[0]["extra"]["route"], Value::from("/charge"));
    }

    /// Anything below ERROR is remembered rather than reported, and arrives attached to the next
    /// event — which is the whole reason to keep it.
    #[test]
    fn lower_levels_become_breadcrumbs_on_the_next_event() {
        let events = reported(|| {
            tracing::info!(target: APP, "charging card");
            tracing::warn!(target: APP, "gateway slow");
            tracing::error!(target: APP, "charge failed");
        });

        assert_eq!(events.len(), 1, "only the error should have been sent");

        let crumbs = events[0]["breadcrumbs"]["values"].as_array().unwrap();
        assert_eq!(crumbs.len(), 2);
        assert_eq!(crumbs[0]["message"], Value::from("charging card"));
        assert_eq!(crumbs[0]["level"], Value::from("info"));
        assert_eq!(crumbs[1]["message"], Value::from("gateway slow"));
        assert_eq!(crumbs[1]["level"], Value::from("warning"));
    }

    /// The loop-breaker. The transport logs a failed delivery at ERROR; reporting that would send
    /// another envelope, which fails, which logs — without bound, and fastest when thermite is
    /// already down.
    #[test]
    fn our_own_logs_are_never_reported() {
        let events = reported(|| {
            tracing::error!(target: "thermite_sdk::transport::http", "could not deliver");
        });

        assert!(events.is_empty());
    }

    /// A record with fields but no message still identifies the line it came from.
    #[test]
    fn a_record_with_no_message_falls_back_to_its_name() {
        let events = reported(|| tracing::error!(target: APP, code = 5));

        assert!(
            events[0]["logentry"]["message"]
                .as_str()
                .unwrap()
                .contains("event"),
            "expected the record's name: {:?}",
            events[0]["logentry"]["message"]
        );
    }
}
