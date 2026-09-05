//! What the native transport does when thermite misbehaves.
//!
//! The contract these protect is the one that matters most and is easiest to lose: **reporting an
//! error must never fail the code path that raised it.** A thermite that is down, wedged, or
//! rejecting every envelope is the normal case at exactly the moment an application is generating
//! the reports worth keeping.
//!
//! Both tests drive a real socket rather than a mock, because what is under test is the blocking
//! HTTP client's behaviour, and a mock transport would assert nothing about it.

// `HttpTransport` only exists with the sender thread; under `web` there is nothing here to test.
#![cfg(feature = "native")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use thermite_sdk::transport::HttpTransport;
use thermite_sdk::{Dsn, Transport};

/// A one-shot server that runs `handle` for each connection, on its own thread.
fn serve(handle: impl Fn(TcpStream) + Send + 'static) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("failed to bind");
    let port = listener.local_addr().unwrap().port();

    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            handle(stream);
        }
    });

    format!("http://abc123@127.0.0.1:{port}/42")
}

fn ingest_url(dsn: &str) -> String {
    Dsn::parse(dsn).expect("invalid DSN").ingest_url
}

fn envelope(n: usize) -> Vec<u8> {
    format!("{{}}\n{{\"type\":\"event\"}}\n{{\"message\":\"{n}\"}}\n").into_bytes()
}

/// A thermite that accepts the connection and then never answers — the shape a hung database or a
/// wedged proxy takes, and worse than a refused connection because it fails slowly.
///
/// `send` must stay non-blocking regardless: it is called from whatever thread just failed, and a
/// reporter that blocks there turns an error into an outage.
#[test]
fn a_wedged_thermite_does_not_block_the_application() {
    let dsn = serve(|stream| {
        // Hold the connection open, reading nothing and writing nothing, until the test ends.
        std::mem::forget(stream);
    });
    let transport = HttpTransport::new(ingest_url(&dsn));

    // Far more than the queue holds, so the drop-when-full branch is the one exercised.
    let started = Instant::now();
    for n in 0..500 {
        transport.send(envelope(n));
    }
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(1),
        "500 sends against a wedged server took {elapsed:?}; send must not block"
    );

    // And the flush is honest about it rather than hanging. Two branches reach the same answer
    // here — the barrier cannot be enqueued because the queue is full, or it is enqueued behind a
    // worker still waiting on the first request — and both must report "did not drain" promptly
    // rather than blocking the caller for `QUEUE_DEPTH * SEND_TIMEOUT`.
    let started = Instant::now();
    assert!(
        !transport.flush(Duration::from_millis(200)),
        "flush should report that the queue did not drain"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "flush overran the timeout it was given"
    );
}

/// A thermite that rejects everything — the shape a wrong DSN key takes, which is a configuration
/// bug rather than a blip. The transport logs it and carries on; what must not happen is the
/// worker dying and taking every later report with it.
#[test]
fn a_rejected_envelope_leaves_the_transport_usable() {
    let dsn = serve(|mut stream| {
        let mut scratch = [0u8; 4096];
        let _ = stream.read(&mut scratch);
        let _ = stream.write_all(
            b"HTTP/1.1 401 Unauthorized\r\nx-sentry-error: bad key\r\ncontent-length: 0\r\n\r\n",
        );
    });
    let transport = HttpTransport::new(ingest_url(&dsn));

    transport.send(envelope(1));
    assert!(
        transport.flush(Duration::from_secs(10)),
        "the rejected send still completed, so the barrier should come back"
    );

    // The worker survived the rejection: a second envelope goes through the same queue.
    transport.send(envelope(2));
    assert!(
        transport.flush(Duration::from_secs(10)),
        "the transport should still be usable after a rejection"
    );
}
