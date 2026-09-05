-- Alert delivery bookkeeping: attempts, backoff, per-channel success, and a cross-replica lease.
--
-- Previously a row was re-offered on every tick until every channel succeeded at once, forever:
-- one permanently failing recipient blocked the queue head, re-spammed the healthy channels every
-- 60 seconds, and after 24 hours the rolling window silently discarded whatever accumulated
-- behind it. And with N replicas, N alert loops delivered every alert N times.
alter table notifications
    add column alert_attempts        int         not null default 0,
    add column alert_next_attempt_at timestamptz,
    -- Dead-lettered: given up after too many attempts. Never claimed again; the triage queue
    -- still carries the row, so the issue itself is not lost.
    add column alert_failed_at       timestamptz,
    -- Per-channel success, so a healthy channel is not re-sent because a sibling failed.
    add column alert_email_at        timestamptz,
    add column alert_webhook_at      timestamptz,
    -- Claim lease, same pattern as the triage queue: one replica delivers a given row.
    add column alert_lease_until     timestamptz;

-- When alerting was first enabled. Rows created before this are never alerted (enabling alerting
-- on a months-old instance must not flood the recipient) — a durable high-water mark rather than
-- a rolling window, so an outage longer than any fixed window loses nothing.
create table alert_state (
    id            int primary key default 1 check (id = 1),
    backlog_floor timestamptz not null
);
