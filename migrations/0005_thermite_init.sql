-- Projects are the unit an SDK reports into. `id` is an integer because Sentry SDKs parse the
-- trailing path segment of a DSN as a numeric project id.
create table projects (
    id         bigserial primary key,
    slug       text        not null unique,
    name       text        not null,
    -- The DSN public key, sent by SDKs as `sentry_key`.
    public_key text        not null unique,
    created_at timestamptz not null default now()
);

-- An issue is a group of events that share a fingerprint. Display columns are denormalized from
-- the most recent event so the issue list needs no join.
create table issues (
    id               bigserial primary key,
    project_id       bigint      not null references projects (id) on delete cascade,
    fingerprint_hash bytea       not null,
    title            text        not null,
    culprit          text,
    exception_type   text,
    exception_value  text,
    level            text        not null,
    platform         text,
    first_seen       timestamptz not null,
    last_seen        timestamptz not null,
    times_seen       bigint      not null default 0,
    status           text        not null default 'unresolved',
    constraint issues_status_check check (status in ('unresolved', 'resolved', 'ignored')),
    constraint issues_fingerprint_unique unique (project_id, fingerprint_hash)
);

create index issues_project_status_last_seen_idx
    on issues (project_id, status, last_seen desc);

create table events (
    id          bigserial primary key,
    -- The SDK-supplied event id. Unique per project, which makes SDK retries idempotent.
    event_id    uuid        not null,
    project_id  bigint      not null references projects (id) on delete cascade,
    issue_id    bigint      not null references issues (id) on delete cascade,
    -- When the error happened, per the SDK.
    timestamp   timestamptz not null,
    -- When we accepted it.
    received_at timestamptz not null default now(),
    level       text        not null,
    platform    text,
    environment text,
    release     text,
    transaction text,
    server_name text,
    message     text,
    -- The full event payload. Everything not promoted to a column above is served from here.
    data        jsonb       not null,
    constraint events_event_id_unique unique (project_id, event_id)
);

create index events_issue_timestamp_idx on events (issue_id, timestamp desc);
