-- Pre-launch waitlist for hosted signup, fed by the landing and pricing pages while
-- THERMITE_WAITLIST is set.
--
-- One row per address, not per click: `email` is the primary key and a repeat submission only
-- bumps `updated_at`, so "how many people are waiting" is a plain count. The address is
-- normalised (trimmed, lowercased) before it gets here — see `models::waitlist`.
create table waitlist (
    email      text        primary key,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index waitlist_created_at_idx on waitlist (created_at);
