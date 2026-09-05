-- Issue-level tag rollup: how many events of an issue carried each tag value.
--
-- Maintained during ingest for the same reason as event_counts: events are dropped by retention,
-- so anything computed from the events table stops being true once eviction starts. This table is
-- the long-term record — "all 400 events had server_name=web-3" stays answerable after the events
-- are gone.
--
-- environment/release/server_name/transaction are synthesized into tags at ingest, so "filter
-- issues by environment" is the same mechanism as filtering by any other tag.
create table issue_tags (
    project_id bigint      not null references projects (id) on delete cascade,
    issue_id   bigint      not null references issues (id) on delete cascade,
    key        text        not null,
    value      text        not null,
    times_seen bigint      not null default 0,
    last_seen  timestamptz not null,
    primary key (issue_id, key, value)
);

-- Drives the per-project tag value listing (e.g. the environment dropdown).
create index issue_tags_project_key_value_idx on issue_tags (project_id, key, value);
