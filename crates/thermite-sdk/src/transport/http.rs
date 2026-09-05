//! The native transport: one background thread that POSTs envelopes.
//!
//! A dedicated OS thread with a blocking client, rather than `tokio::spawn` with an async one.
//! Reporting has to work in a binary with no async runtime, and a flush at shutdown must not
//! depend on one still being alive — a runtime that has begun shutting down silently drops the
//! task that was going to deliver the last event. One thread costs a stack and removes both
//! problems.
//!
//! There is no retry. An envelope that fails to send is logged and dropped, because a retry queue
//! needs backoff, a deadline and a bound to avoid becoming an unbounded buffer of stale events —
//! which is the thing thermite's synchronous ingest exists to avoid. If retries turn out to be
//! wanted, they belong here behind a bounded queue, not in the caller.

use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::time::Duration;

use crate::transport::Transport;

/// Envelopes that may wait for the sender.
///
/// Small on purpose: a backlog this deep already means the network is gone, and holding more
/// events would trade the process's memory for reports nobody is waiting on.
const QUEUE_DEPTH: usize = 64;

/// How long one POST may take before it is abandoned.
///
/// Thermite's ingest is synchronous — it writes to Postgres before acknowledging — so a slow
/// database shows up here as a slow response, and this is the ceiling on how long that may stall
/// the queue behind it.
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

enum Task {
    Send(Vec<u8>),
    /// A flush barrier. The channel is FIFO, so an ack for this proves every envelope queued
    /// before it has already been through `post`.
    Flush(SyncSender<()>),
}

pub struct HttpTransport {
    tasks: SyncSender<Task>,
}

impl HttpTransport {
    /// Starts the sender thread. `url` is `Dsn::ingest_url`, credentials included.
    pub fn new(url: String) -> Self {
        let (tasks, queue) = sync_channel(QUEUE_DEPTH);

        // Named so it is identifiable in a thread dump: an SDK thread showing up in a profile of
        // someone else's application should say whose it is.
        let spawned = std::thread::Builder::new()
            .name("thermite-sdk".to_string())
            .spawn(move || worker(url, queue));

        if let Err(error) = spawned {
            tracing::error!(%error, "could not start the thermite-sdk sender thread");
        }

        Self { tasks }
    }
}

impl Transport for HttpTransport {
    fn send(&self, envelope: Vec<u8>) {
        match self.tasks.try_send(Task::Send(envelope)) {
            Ok(()) => {}
            // Dropping the newest event is the right end to drop from: the ones already queued are
            // older, and an error storm's first events are the informative ones.
            Err(TrySendError::Full(_)) => {
                tracing::warn!("thermite-sdk queue is full, dropping an event")
            }
            Err(TrySendError::Disconnected(_)) => {
                tracing::error!("thermite-sdk sender thread is gone, dropping an event")
            }
        }
    }

    fn flush(&self, timeout: Duration) -> bool {
        let (ack, done) = sync_channel(1);

        // `try_send`, not `send`: a blocking enqueue could wait `QUEUE_DEPTH * SEND_TIMEOUT`
        // before the barrier is even accepted, which is not the timeout the caller asked for.
        if self.tasks.try_send(Task::Flush(ack)).is_err() {
            return false;
        }

        done.recv_timeout(timeout).is_ok()
    }
}

fn worker(url: String, queue: Receiver<Task>) {
    let client = match reqwest::blocking::Client::builder()
        .timeout(SEND_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            tracing::error!(%error, "could not build the thermite-sdk http client");
            return;
        }
    };

    // Ends when the sender is dropped, which is what stops the thread.
    for task in queue {
        match task {
            Task::Send(envelope) => post(&client, &url, envelope),
            Task::Flush(ack) => {
                let _ = ack.send(());
            }
        }
    }
}

fn post(client: &reqwest::blocking::Client, url: &str, envelope: Vec<u8>) {
    let sent = client
        .post(url)
        .header("content-type", "application/x-sentry-envelope")
        .body(envelope)
        .send();

    match sent {
        Ok(response) if response.status().is_success() => {}
        // 401 means the DSN key is wrong and every later event will fail the same way, so it is
        // logged at error: it is a configuration bug, not a blip.
        Ok(response) if response.status() == 401 => {
            tracing::error!("thermite rejected the DSN key, no events will be recorded")
        }
        Ok(response) => {
            tracing::warn!(status = %response.status(), "thermite rejected an envelope")
        }
        Err(error) => tracing::warn!(%error, "could not deliver an envelope to thermite"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing here reaches the network: the point is that a transport pointed at an unroutable
    /// address still accepts events and still returns from `flush`, because an unreachable error
    /// tracker must not be able to hang the application reporting into it.
    #[test]
    fn an_unreachable_endpoint_neither_blocks_nor_panics() {
        let transport = HttpTransport::new("http://127.0.0.1:1/api/1/envelope/".to_string());

        transport.send(b"{}\n{\"type\":\"event\"}\n{}\n".to_vec());

        assert!(
            transport.flush(SEND_TIMEOUT),
            "the barrier should come back once the failed send is done"
        );
    }

    /// A flush with nothing queued is the shutdown path of a process that never errored.
    #[test]
    fn flushing_an_idle_transport_returns_immediately() {
        let transport = HttpTransport::new("http://127.0.0.1:1/api/1/envelope/".to_string());

        assert!(transport.flush(Duration::from_secs(5)));
    }
}
