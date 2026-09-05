-- Pre-aggregated event counts, one row per issue / hour / level.
--
-- Every rate chart, sparkline and "events today" figure reads from here rather than from `events`.
-- Counting raw rows works fine at four events and falls over at four million, and the counters
-- cannot be reconstructed once old events are dropped under a retention policy — so the rollup has
-- to exist before there is data worth charting, not after.
--
-- Bucketed on the event's own `timestamp`, not on `received_at`: "error rate" means when errors
-- happened, not when they reached us. Timestamps more than an hour in our future are already
-- clamped at ingest, so a bad client clock cannot push counts into buckets that do not exist yet.
create table event_counts (
    project_id bigint      not null references projects (id) on delete cascade,
    issue_id   bigint      not null references issues (id) on delete cascade,
    -- Truncated to the hour. Hourly is the finest resolution any chart needs (24 points for a day,
    -- 168 for a week) and keeps rows per issue bounded at ~8.7k per year.
    bucket     timestamptz not null,
    level      text        not null,
    count      bigint      not null default 0,

    primary key (issue_id, bucket, level)
);

-- Project-wide series and totals, which do not filter by issue.
create index event_counts_project_bucket_idx on event_counts (project_id, bucket desc);
