-- Bound the rollups that retention deliberately preserves.
--
-- issue_tags and event_counts grow with data cardinality, not with time, so "retention bounds
-- disk" was only true for the events table. Three changes:
--
-- 1. users_affected becomes a counter on issues, maintained at ingest when a never-before-seen
--    user value is recorded. The issue list used to count(*) over issue_tags per render — the
--    exact anti-pattern the event_counts rollup exists to avoid — and a counter also stays true
--    after old tag rows are pruned, which count(*) would not.
alter table issues
    add column users_affected bigint not null default 0;

-- Backfill from the rollup as it stands today.
update issues
   set users_affected = coalesce(
       (select count(*) from issue_tags t where t.issue_id = issues.id and t.key = 'user'), 0);

-- 2. The retention sweep now prunes rollup rows past the age policy; these are the scans it runs.
create index issue_tags_last_seen_idx on issue_tags (last_seen);
create index event_counts_bucket_idx on event_counts (bucket);
