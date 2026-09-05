-- Per-project alert routing.
--
-- Null falls back to the instance-wide THERMITE_ALERT_EMAIL / THERMITE_ALERT_WEBHOOK; a value
-- replaces the global setting for this project's alerts (routing, not fan-out).
alter table projects
    add column alert_email   text,
    add column alert_webhook text;
