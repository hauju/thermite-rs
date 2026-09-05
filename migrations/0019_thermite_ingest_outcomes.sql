-- Ingest outcome counters, one row per project / hour / outcome.
--
-- The quota and the envelope parser drop things by design — over-quota events, unsupported item
-- types, unparseable payloads — and until now the only trace was a debug log line. These counters
-- exist to answer "am I silently losing events?" from the dashboard, which is exactly the question
-- quota enforcement creates.
--
-- Same shape and rationale as `event_counts`: a rollup maintained at ingest, because a row per
-- dropped request is unbounded and the counts cannot be reconstructed later (a dropped event
-- leaves nothing else behind).
--
-- Bucketed on `received_at` (the server clock), not a client timestamp: a rejected request's
-- payload is untrusted or never parsed at all, and outcomes measure ingest behavior, not when
-- errors happened.
create table ingest_outcomes (
    project_id bigint      not null references projects (id) on delete cascade,
    -- Truncated to the hour, like event_counts.
    bucket     timestamptz not null,
    -- 'accepted' | 'over_quota' | 'unsupported' | 'invalid' (see thermite_core::ingest::outcomes).
    outcome    text        not null,
    count      bigint      not null default 0,

    primary key (project_id, bucket, outcome)
);

-- Retention ages buckets out globally, like the other rollups.
create index ingest_outcomes_bucket_idx on ingest_outcomes (bucket);
