//! The synthetic error kinds the playground page can raise.
//!
//! Shared rather than duplicated: the page renders one button per kind and the server function
//! matches on `id` to pick a payload, so a kind added here appears in both without either side
//! drifting. Only the metadata is shared — the payloads themselves are server-only, in
//! `crate::server::demo_events`.

/// One button on the playground page.
pub struct DemoKind {
    /// Sent to the server function; matched there to choose a payload.
    pub id: &'static str,
    pub label: &'static str,
    /// What the resulting issue demonstrates, so the page explains itself.
    pub description: &'static str,
}

pub const DEMO_KINDS: &[DemoKind] = &[
    DemoKind {
        id: "exception",
        label: "Unhandled exception",
        description: "A TypeError with a stack trace. The culprit is taken from the innermost \
                      in-app frame, not the library frame that raised.",
    },
    DemoKind {
        id: "db_timeout",
        label: "Database timeout",
        description: "The host and duration differ on every click, yet all occurrences collapse \
                      into one issue — that is the grouping normalizer at work.",
    },
    DemoKind {
        id: "log_message",
        label: "Log message",
        description: "Error level, no exception. Groups on the normalized message text instead of \
                      an exception type.",
    },
    DemoKind {
        id: "fingerprint",
        label: "Custom fingerprint",
        description: "Two unrelated exception types pinned to a single issue by an explicit \
                      fingerprint, the way you would group one user-facing failure.",
    },
    DemoKind {
        id: "warning",
        label: "Warning",
        description: "A warning-level event, so the level column and filters have something to \
                      separate from errors.",
    },
];
