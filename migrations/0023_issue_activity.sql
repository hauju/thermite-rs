-- What happened to an issue, in order: every status change a person or agent made, and every
-- reopen by ingest. Analyses and notes already live in issue_analyses and are merged in on read,
-- so this table holds only what has no home of its own.
create table issue_activity (
    id         bigserial   primary key,
    issue_id   bigint      not null references issues (id) on delete cascade,
    -- 'status' for a change somebody made; 'regression' for a reopen by ingest.
    kind       text        not null,
    -- Who: a dashboard user's name, 'api' for a REST caller, an MCP caller's name. Null for
    -- ingest, which acts for nobody.
    actor      text,
    -- 'status': {"from", "to", "in_next_release", "release"} — release being the anchor when
    -- resolved until the next one. 'regression': {"release", "regressed_from"}.
    detail     jsonb       not null default '{}'::jsonb,
    created_at timestamptz not null default now(),

    constraint issue_activity_kind_check check (kind in ('status', 'regression'))
);

create index issue_activity_issue_idx on issue_activity (issue_id, id);
