//! A minimal Sentry-protocol reporter for Thermite.
//!
//! Thermite speaks Sentry's wire protocol, so unmodified Sentry SDKs work against it and always
//! will — `an_unmodified_sentry_sdk_reports_into_thermite` is the standing proof. This crate exists
//! because the *cost* of those SDKs is out of proportion to what thermite reads: seventeen
//! `sentry-*` crates, a hub-and-scope stack, and a transport layer, to fill in about fourteen
//! fields.
//!
//! So this models exactly the fields thermite indexes and nothing else:
//!
//! | Field | Read by |
//! |---|---|
//! | `event_id`, `timestamp`, `level` | `thermite_core::ingest::digest` |
//! | `platform`, `environment`, `release`, `transaction`, `server_name` | `ingest::digest` |
//! | `tags`, `user` | `ingest::digest`, `protocol::event::user_key` |
//! | `exception[].type` / `.value` / `.stacktrace` | `protocol::grouping` |
//! | `fingerprint` | `protocol::grouping` |
//! | `logentry` | `protocol::event::type_and_value` |
//! | `breadcrumbs`, `contexts` | the issue detail page and the `get_issue` MCP tool |
//!
//! Everything else in the Sentry event schema is stored verbatim and never looked at, which is
//! reason enough not to send it.
//!
//! # Usage
//!
//! ```no_run
//! # fn main() -> Result<(), thermite_sdk::DsnError> {
//! let mut options = thermite_sdk::Options::new(std::env::var("THERMITE_DSN").unwrap());
//! options.release = Some(env!("CARGO_PKG_VERSION").to_string());
//!
//! // Held for the life of `main`: dropping it flushes what the sender thread still has queued.
//! let _guard = thermite_sdk::init(options)?;
//! # Ok(())
//! # }
//! ```
//!
//! # One client, no hubs
//!
//! There is a single process-wide client and no scope stack. That machinery is most of what makes
//! the Sentry SDKs large, and it buys thread-local context propagation that a process reporting
//! its own errors does not need.
//!
//! # Relationship to `thermite-core`
//!
//! None, deliberately. `thermite-core` *reads* envelopes and drags in sqlx and axum to do it; this
//! crate *writes* them and has to build for `wasm32-unknown-unknown`. The overlap is a list of
//! field names, which is not enough shared vocabulary to justify a third crate between them. What
//! keeps the two honest is a test that sends through both this SDK and an unmodified Sentry
//! client and asserts the same grouping hash comes out — not a shared type.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock, PoisonError, RwLock};
use std::time::Duration;

use serde_json::Value;
use uuid::Uuid;

pub mod contexts;
pub mod dsn;
pub mod envelope;
pub mod event;
pub mod panic;
pub mod scope;
#[cfg(feature = "sessions")]
pub mod session;
#[cfg(feature = "native")]
pub mod stacktrace;
#[cfg(feature = "tracing-layer")]
pub mod tracing_layer;
pub mod transport;

pub use dsn::{Dsn, DsnError};
pub use event::{Breadcrumb, Event, Exception, Frame, Level, Stacktrace, User};
pub use scope::Scope;
pub use transport::{TestTransport, Transport};

/// How long `Guard::drop` waits for the queue to drain.
///
/// Short enough not to noticeably delay a shutdown that has nothing to send, long enough to cover
/// one round trip to a healthy thermite.
const SHUTDOWN_FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

static CLIENT: OnceLock<Client> = OnceLock::new();

/// What the client stamps onto every event.
pub struct Options {
    pub dsn: String,
    /// The version this build reports as. Release health and regression detection are both
    /// release-scoped, so without it thermite can group errors but cannot tell you what broke.
    pub release: Option<String>,
    pub environment: Option<String>,
    pub server_name: Option<String>,
    /// Report panics. On by default: an unreported panic is the failure most worth knowing about.
    pub attach_panic_hook: bool,
    /// Breadcrumbs retained, and therefore sent on every event.
    pub max_breadcrumbs: usize,
    /// Open a session at `init` and close it when the guard drops.
    ///
    /// On by default. Sessions cost no quota in thermite and the crash-free rate is unavailable
    /// without them, so the choice is between release health working and it reading "not enough
    /// data" forever.
    #[cfg(feature = "sessions")]
    pub auto_session_tracking: bool,
}

impl Options {
    pub fn new(dsn: impl Into<String>) -> Self {
        Self {
            dsn: dsn.into(),
            release: None,
            environment: None,
            server_name: None,
            attach_panic_hook: true,
            max_breadcrumbs: scope::DEFAULT_MAX_BREADCRUMBS,
            #[cfg(feature = "sessions")]
            auto_session_tracking: true,
        }
    }
}

/// A configured reporter.
///
/// [`init`] installs one process-wide and the free functions below go through it, which is what
/// instrumentation scattered across a codebase wants. Holding one directly is for the cases that
/// global cannot serve: two DSNs in one process, and tests, where a `OnceLock` would let whichever
/// case ran first decide for all the others.
pub struct Client {
    dsn: Dsn,
    transport: Arc<dyn Transport>,
    release: Option<String>,
    environment: Option<String>,
    server_name: Option<String>,
    /// Read once at construction — the host does not change under a running process.
    contexts: BTreeMap<String, Value>,
    scope: RwLock<Scope>,
    #[cfg(feature = "sessions")]
    session: std::sync::Mutex<Option<session::Session>>,
}

impl Client {
    /// Builds a client that reports through `transport`.
    pub fn with_transport(
        options: Options,
        transport: Arc<dyn Transport>,
    ) -> Result<Self, DsnError> {
        Ok(Self {
            dsn: Dsn::parse(&options.dsn)?,
            transport,
            release: options.release,
            environment: options.environment,
            server_name: options.server_name,
            contexts: contexts::describe(),
            scope: RwLock::new(Scope::new(options.max_breadcrumbs)),
            #[cfg(feature = "sessions")]
            session: std::sync::Mutex::new(None),
        })
    }

    /// Builds a client that reports over HTTP, from a background sender thread.
    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    pub fn new(options: Options) -> Result<Self, DsnError> {
        let dsn = Dsn::parse(&options.dsn)?;
        Self::with_transport(
            options,
            Arc::new(transport::HttpTransport::new(dsn.ingest_url)),
        )
    }

    /// Builds a client that reports with `fetch`.
    #[cfg(all(target_arch = "wasm32", feature = "web"))]
    pub fn new(options: Options) -> Result<Self, DsnError> {
        let dsn = Dsn::parse(&options.dsn)?;
        Self::with_transport(
            options,
            Arc::new(transport::FetchTransport::new(dsn.ingest_url)),
        )
    }

    /// Reports an event, returning the id thermite will file it under.
    pub fn capture(&self, mut event: Event) -> Uuid {
        // Only an actual error makes a session "errored". A warning or an info message says
        // something happened, not that the run went wrong, and counting those would put the
        // crash-free rate on the floor of every chatty process.
        #[cfg(feature = "sessions")]
        if matches!(event.level, Level::Fatal | Level::Error)
            && let Some(session) = self.locked_session().as_mut()
        {
            session.record_error();
        }

        self.read_scope().apply(&mut event);

        // Filled in only where the event did not say. An event that set its own release means it,
        // which is what makes a build reporting on behalf of another one possible.
        if event.release.is_none() {
            event.release.clone_from(&self.release);
        }
        if event.environment.is_none() {
            event.environment.clone_from(&self.environment);
        }
        if event.server_name.is_none() {
            event.server_name.clone_from(&self.server_name);
        }
        for (key, value) in &self.contexts {
            event
                .contexts
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }

        let event_id = event.event_id;
        match envelope::event_envelope(&event, &self.dsn) {
            Ok(body) => self.transport.send(body),
            // Serializing cannot fail for the types in `event`, but returning the id regardless is
            // better than a panic inside the reporting path of some other failure.
            Err(error) => tracing::warn!(%error, "could not serialize an event"),
        }
        event_id
    }

    /// Edits the tags, user and breadcrumbs stamped onto every later event.
    pub fn configure_scope(&self, edit: impl FnOnce(&mut Scope)) {
        edit(&mut self.scope.write().unwrap_or_else(PoisonError::into_inner));
    }

    /// Blocks until queued events are sent, or `timeout` expires.
    pub fn flush(&self, timeout: Duration) -> bool {
        self.transport.flush(timeout)
    }

    /// Opens a release-health session.
    ///
    /// A no-op without a release: thermite drops a session that names none, because the rollup is
    /// keyed on the release row. Warning rather than silence — a caller that asked for sessions
    /// and configured no release wants to know why the dashboard stays empty.
    #[cfg(feature = "sessions")]
    pub fn start_session(&self) {
        let Some(release) = self.release.clone() else {
            tracing::warn!("no release is configured, so no session was started");
            return;
        };

        let session = session::Session::new(release, self.environment.clone());
        self.send_session(&session.opened());
        *self.locked_session() = Some(session);
    }

    /// Closes the session, sending the terminal update thermite reads the outcome from.
    ///
    /// Idempotent: the session is taken, so a second call — the guard dropping after an explicit
    /// end — sends nothing and cannot double-count.
    #[cfg(feature = "sessions")]
    pub fn end_session(&self) {
        let ended = self.locked_session().take();
        if let Some(session) = ended {
            self.send_session(&session.closed());
        }
    }

    /// Marks the running session as having crashed rather than exited.
    #[cfg(feature = "sessions")]
    pub fn mark_session_crashed(&self) {
        if let Some(session) = self.locked_session().as_mut() {
            session.mark_crashed();
        }
    }

    #[cfg(feature = "sessions")]
    fn send_session(&self, update: &session::Update) {
        match envelope::session_envelope(update, &self.dsn) {
            Ok(body) => self.transport.send(body),
            Err(error) => tracing::warn!(%error, "could not serialize a session update"),
        }
    }

    #[cfg(feature = "sessions")]
    fn locked_session(&self) -> std::sync::MutexGuard<'_, Option<session::Session>> {
        self.session.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A poisoned scope lock is recovered rather than propagated: a panic while holding it leaves
    /// the scope readable, and refusing to report *because* something already panicked is exactly
    /// backwards.
    fn read_scope(&self) -> std::sync::RwLockReadGuard<'_, Scope> {
        self.scope.read().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Starts reporting through the given transport.
///
/// Prefer [`init`], which builds the native one. This is the entry point for a target that needs
/// its own — and the one tests use, with [`TestTransport`].
pub fn init_with(options: Options, transport: Arc<dyn Transport>) -> Result<Guard, DsnError> {
    let attach_panic_hook = options.attach_panic_hook;
    #[cfg(feature = "sessions")]
    let auto_session_tracking = options.auto_session_tracking;

    if CLIENT
        .set(Client::with_transport(options, transport)?)
        .is_err()
    {
        // Not an error: a second `init` is usually a test or a library initializing defensively,
        // and tearing down the working client to install a duplicate would be worse.
        tracing::warn!("thermite-sdk is already initialized, ignoring this call");
        return Ok(Guard { _private: () });
    }

    if attach_panic_hook {
        panic::install();
    }
    #[cfg(feature = "sessions")]
    if auto_session_tracking {
        start_session();
    }

    Ok(Guard { _private: () })
}

/// Starts reporting to the DSN, installing the client this module's free functions use.
///
/// On wasm the guard's flush is a no-op — `keepalive` is what carries a report past the page's
/// lifetime, so there is no queue to drain at shutdown.
#[cfg(any(
    all(feature = "native", not(target_arch = "wasm32")),
    all(target_arch = "wasm32", feature = "web")
))]
pub fn init(options: Options) -> Result<Guard, DsnError> {
    // Parsed twice — once here for the transport's URL, once inside the client. It is a few dozen
    // bytes at startup, and it keeps the install path in one place rather than two that must stay
    // in step.
    let dsn = Dsn::parse(&options.dsn)?;

    #[cfg(not(target_arch = "wasm32"))]
    let transport = Arc::new(transport::HttpTransport::new(dsn.ingest_url));
    #[cfg(target_arch = "wasm32")]
    let transport = Arc::new(transport::FetchTransport::new(dsn.ingest_url));

    init_with(options, transport)
}

/// Reports an event, returning the id thermite will file it under.
///
/// `None` when no client is initialized, which is what an application with no DSN configured looks
/// like. Capturing before `init` is a no-op rather than an error: instrumentation should not have
/// to know whether reporting is switched on.
pub fn capture_event(event: Event) -> Option<Uuid> {
    CLIENT.get().map(|client| client.capture(event))
}

/// Reports a log message. Titles as `"Log Message: <first line>"`.
pub fn capture_message(message: impl Into<String>, level: Level) -> Option<Uuid> {
    capture_event(Event::message(message, level))
}

/// Reports an error and the chain of causes underneath it.
///
/// Where a stack is available it is the stack *here*, not where the error was constructed. A Rust
/// error is a value and carries no record of its own creation; sentry-rust reports the same thing
/// for the same reason. Capture close to the failure and the two coincide.
pub fn capture_error(error: &dyn std::error::Error) -> Option<Uuid> {
    capture_event(with_stacktrace(Event::from_error(error)))
}

/// Attaches the current stack to the outermost exception — the one thermite titles and groups the
/// issue on, so the one the stack belongs to.
#[cfg(feature = "native")]
fn with_stacktrace(mut event: Event) -> Event {
    if let Some(exception) = event
        .exception
        .as_mut()
        .and_then(|chain| chain.values.last_mut())
    {
        exception.stacktrace = Some(stacktrace::capture());
    }
    event
}

/// No stack to walk without the native backend, so the event passes through unchanged.
#[cfg(not(feature = "native"))]
fn with_stacktrace(event: Event) -> Event {
    event
}

/// Edits the tags, user and breadcrumbs stamped onto every later event.
///
/// A no-op before `init`, like the capture functions: instrumentation should not have to know
/// whether reporting is switched on.
pub fn configure_scope(edit: impl FnOnce(&mut Scope)) {
    if let Some(client) = CLIENT.get() {
        client.configure_scope(edit);
    }
}

/// Records a breadcrumb, dropping the oldest once `Options::max_breadcrumbs` is reached.
pub fn add_breadcrumb(breadcrumb: Breadcrumb) {
    configure_scope(|scope| scope.add_breadcrumb(breadcrumb));
}

/// Opens a release-health session. `init` does this already unless asked not to.
#[cfg(feature = "sessions")]
pub fn start_session() {
    if let Some(client) = CLIENT.get() {
        client.start_session();
    }
}

/// Closes the release-health session. The guard does this on drop; calling it twice is harmless.
#[cfg(feature = "sessions")]
pub fn end_session() {
    if let Some(client) = CLIENT.get() {
        client.end_session();
    }
}

#[cfg(feature = "sessions")]
pub(crate) fn mark_session_crashed() {
    if let Some(client) = CLIENT.get() {
        client.mark_session_crashed();
    }
}

/// Blocks until queued events are sent, or `timeout` expires. Returns whether the queue drained.
///
/// True when nothing is initialized: there is nothing undelivered.
pub fn flush(timeout: Duration) -> bool {
    CLIENT
        .get()
        .is_none_or(|client| client.transport.flush(timeout))
}

/// Flushes on drop.
///
/// The sender runs on its own thread, so an event captured moments before `main` returns is still
/// queued when the process exits. Holding this for the life of `main` is what turns that into a
/// delivered event.
pub struct Guard {
    _private: (),
}

impl Drop for Guard {
    fn drop(&mut self) {
        // Before the flush, so the terminal session update is in the queue the flush drains.
        #[cfg(feature = "sessions")]
        end_session();

        flush(SHUTDOWN_FLUSH_TIMEOUT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A client wired to a recorder, bypassing the process-wide `CLIENT`.
    ///
    /// `OnceLock` means only one test could ever call `init_with`, so everything that is really
    /// about `capture` is tested against a client held locally instead.
    fn client(recorder: &Arc<TestTransport>) -> Client {
        let mut options = Options::new("http://abc123@localhost:9000/42");
        options.release = Some("1.4.2".to_string());
        options.environment = Some("production".to_string());
        options.server_name = Some("worker-1".to_string());

        Client::with_transport(options, recorder.clone()).unwrap()
    }

    fn captured(recorder: &Arc<TestTransport>) -> serde_json::Value {
        let body = recorder.bodies().remove(0);
        serde_json::from_str(body.lines().nth(2).unwrap()).unwrap()
    }

    #[test]
    fn an_event_inherits_the_client_defaults() {
        let recorder = Arc::new(TestTransport::new());
        client(&recorder).capture(Event::message("gateway down", Level::Error));

        let event = captured(&recorder);
        assert_eq!(event["release"], serde_json::Value::from("1.4.2"));
        assert_eq!(event["environment"], serde_json::Value::from("production"));
        assert_eq!(event["server_name"], serde_json::Value::from("worker-1"));
    }

    /// An event that names its own release means it — a deploy tool reporting on behalf of the
    /// build it just shipped is not reporting about itself.
    #[test]
    fn an_event_keeps_a_release_it_set_itself() {
        let recorder = Arc::new(TestTransport::new());
        let mut event = Event::message("gateway down", Level::Error);
        event.release = Some("2.0.0".to_string());

        client(&recorder).capture(event);

        assert_eq!(
            captured(&recorder)["release"],
            serde_json::Value::from("2.0.0")
        );
    }

    /// The returned id is what the caller correlates against, so it has to be the one that went
    /// out — not a fresh one minted on the way back.
    #[test]
    fn capture_returns_the_id_that_went_on_the_wire() {
        let recorder = Arc::new(TestTransport::new());
        let event = Event::message("gateway down", Level::Error);
        let expected = event.event_id;

        let returned = client(&recorder).capture(event);

        assert_eq!(returned, expected);
        assert!(recorder.bodies()[0].contains(&expected.simple().to_string()));
    }

    #[test]
    fn an_error_captures_its_whole_chain() {
        let recorder = Arc::new(TestTransport::new());
        let io = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "connection refused");

        client(&recorder).capture(Event::from_error(&io));

        let event = captured(&recorder);
        assert_eq!(
            event["exception"]["values"][0]["value"],
            serde_json::Value::from("connection refused")
        );
    }

    /// The `get_issue` MCP tool promises runtime contexts, so they have to ride on every event
    /// rather than being something the caller remembers to attach.
    #[test]
    fn an_event_carries_the_host_contexts() {
        let recorder = Arc::new(TestTransport::new());
        client(&recorder).capture(Event::message("gateway down", Level::Error));

        let event = captured(&recorder);
        assert_eq!(
            event["contexts"]["os"]["name"],
            serde_json::Value::from(std::env::consts::OS)
        );
    }

    /// An event that describes its own context wins. Nothing does that yet, but silently
    /// overwriting what a caller set would be the wrong default to bake in.
    #[test]
    fn an_event_keeps_a_context_it_set_itself() {
        let recorder = Arc::new(TestTransport::new());
        let mut event = Event::message("gateway down", Level::Error);
        event
            .contexts
            .insert("os".to_string(), serde_json::json!({ "name": "plan9" }));

        client(&recorder).capture(event);

        assert_eq!(
            captured(&recorder)["contexts"]["os"]["name"],
            serde_json::Value::from("plan9")
        );
    }

    #[cfg(feature = "sessions")]
    mod sessions {
        use super::*;

        /// The item header, and the payload, of each envelope handed to the transport.
        fn items(recorder: &Arc<TestTransport>) -> Vec<(String, serde_json::Value)> {
            recorder
                .bodies()
                .iter()
                .map(|body| {
                    let mut lines = body.lines().skip(1);
                    let headers: serde_json::Value =
                        serde_json::from_str(lines.next().unwrap()).unwrap();
                    let payload = serde_json::from_str(lines.next().unwrap()).unwrap();
                    (headers["type"].as_str().unwrap().to_string(), payload)
                })
                .collect()
        }

        /// Exactly two session updates per run — the `init` thermite counts as a start, and the
        /// terminal status it reads the outcome from. A third would inflate the totals.
        #[test]
        fn a_run_sends_one_opening_and_one_closing_update() {
            let recorder = Arc::new(TestTransport::new());
            let client = client(&recorder);

            client.start_session();
            client.capture(Event::message("gateway down", Level::Error));
            client.end_session();

            let items = items(&recorder);
            let kinds: Vec<&str> = items.iter().map(|(kind, _)| kind.as_str()).collect();
            assert_eq!(kinds, vec!["session", "event", "session"]);

            assert_eq!(items[0].1["init"], serde_json::Value::from(true));
            assert_eq!(items[0].1["status"], serde_json::Value::from("ok"));
            assert_eq!(
                items[0].1["attrs"]["release"],
                serde_json::Value::from("1.4.2")
            );

            assert_eq!(items[2].1["init"], serde_json::Value::from(false));
            assert_eq!(items[2].1["status"], serde_json::Value::from("exited"));
            assert_eq!(items[2].1["errors"], serde_json::Value::from(1));
        }

        /// Both updates have to bucket together, or the hour that got the outcome without the
        /// total reports a rate above 100%.
        #[test]
        fn both_updates_carry_the_same_start_time() {
            let recorder = Arc::new(TestTransport::new());
            let client = client(&recorder);

            client.start_session();
            client.end_session();

            let items = items(&recorder);
            assert_eq!(items[0].1["started"], items[1].1["started"]);
            assert_eq!(items[0].1["sid"], items[1].1["sid"]);
        }

        /// The guard drops after an explicit `end_session`, so ending twice has to be free.
        #[test]
        fn ending_a_session_twice_sends_one_terminal_update() {
            let recorder = Arc::new(TestTransport::new());
            let client = client(&recorder);

            client.start_session();
            client.end_session();
            client.end_session();

            assert_eq!(recorder.len(), 2);
        }

        /// Only real errors count. A chatty process logging warnings is not a broken release.
        #[test]
        fn a_warning_does_not_make_the_run_errored() {
            let recorder = Arc::new(TestTransport::new());
            let client = client(&recorder);

            client.start_session();
            client.capture(Event::message("slow response", Level::Warning));
            client.end_session();

            assert_eq!(items(&recorder)[2].1["errors"], serde_json::Value::from(0));
        }

        #[test]
        fn a_crash_closes_the_session_as_crashed() {
            let recorder = Arc::new(TestTransport::new());
            let client = client(&recorder);

            client.start_session();
            client.capture(Event::message("boom", Level::Fatal));
            client.mark_session_crashed();
            client.end_session();

            assert_eq!(
                items(&recorder)[2].1["status"],
                serde_json::Value::from("crashed")
            );
        }

        /// Thermite drops a session naming no release, so sending one would be pure traffic.
        #[test]
        fn a_client_with_no_release_starts_no_session() {
            let recorder = Arc::new(TestTransport::new());
            let client = Client::with_transport(
                Options::new("http://abc123@localhost:9000/42"),
                recorder.clone(),
            )
            .unwrap();

            client.start_session();
            client.end_session();

            assert!(recorder.is_empty());
        }
    }

    /// Instrumentation runs whether or not a DSN was configured, so the uninitialized path has to
    /// be a no-op rather than a panic.
    #[test]
    fn capturing_without_a_client_is_a_no_op() {
        assert_eq!(capture_message("nobody is listening", Level::Error), None);
        assert!(flush(Duration::from_millis(1)));
    }
}
