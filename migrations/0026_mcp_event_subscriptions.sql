-- MCP event subscriptions (the experimental `events/*` extension ChatGPT speaks): a client asks
-- to be told when an issue is created or regresses, and names a webhook to be told at.
--
-- `id` is derived, not generated: a hash over (principal, callback_url, event_name, arguments),
-- so a repeated `events/subscribe` lands on the same row and refreshes it instead of stacking a
-- duplicate. `principal` is opaque to thermite-core — the application decides what identifies a
-- caller.
--
-- `secret_ciphertext` is the client's `whsec_` signing secret, encrypted by the application. It
-- cannot be hashed: every delivery is signed with it.
--
-- `expires_at` is the lease the client must refresh before. Expired rows are skipped when
-- firing, not swept.
create table event_subscriptions (
    id                text        primary key,
    principal         text        not null,
    event_name        text        not null,
    arguments         jsonb       not null,
    callback_url      text        not null,
    secret_ciphertext text        not null,
    expires_at        timestamptz not null,
    created_at        timestamptz not null default now(),
    updated_at        timestamptz not null default now()
);

-- Firing reads every live subscription to one event name.
create index event_subscriptions_event_name_expires_at_idx
    on event_subscriptions (event_name, expires_at);
