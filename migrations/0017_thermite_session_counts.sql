-- Release health: how many sessions a release ran, and how many of them ended badly.
--
-- Same rationale as event_counts — a row per session is unbounded, and retention would erase the
-- history anyway, so the rate is maintained at ingest and this rollup is the permanent record.
--
-- Keyed on releases.id rather than a version string: releases is already capped at 10k per
-- project (MAX_RELEASES_PER_PROJECT), so that cap is what bounds this table too. A session
-- carrying no release is dropped at ingest — release health that cannot be attributed to a
-- release answers no question, and accepting one would mint rows under a null key forever.
--
-- There is deliberately no `environment` column. It is client-controlled and unbounded, and it is
-- the one dimension that would let an SDK mint rollup rows without limit. Adding it later is an
-- additive migration; getting its cardinality cap wrong now is permanent storage growth.
create table session_counts (
    project_id bigint      not null references projects (id) on delete cascade,
    release_id bigint      not null references releases (id) on delete cascade,
    -- Truncated to the hour, from the session's *start*, never from the update that closed it.
    -- A session beginning at 10:59 and crashing at 11:01 must put its total and its crash in the
    -- same bucket, or that bucket reports a crash rate above 100%.
    bucket     timestamptz not null,

    -- Totals, not states: `sessions` counts every session that began in this bucket, and the
    -- three below count how they ended. A session still running is counted in `sessions` alone.
    sessions   bigint      not null default 0,
    errored    bigint      not null default 0,
    crashed    bigint      not null default 0,
    abnormal   bigint      not null default 0,

    primary key (release_id, bucket)
);

-- The per-project release list, newest buckets first.
create index session_counts_project_bucket_idx on session_counts (project_id, bucket desc);
-- The retention sweep's scan.
create index session_counts_bucket_idx on session_counts (bucket);
