-- Cron monitoring: scheduled jobs that report when they run, so *not* running is detectable.
--
-- Sentry's check-in protocol. A job sends `check_in` envelope items on the same ingest endpoint
-- as errors, authenticating with the same DSN key; the monitor is created on first sighting and
-- carries its schedule so a missed run can be noticed without anyone configuring anything twice.
--
-- A missed or overrunning job is turned into a normal error event by the sweeper, so grouping,
-- alerting, triage and retention all apply unchanged — cron monitoring adds a *source* of events,
-- not a second pipeline beside them.
create table monitors (
    id         bigserial primary key,
    project_id bigint not null references projects (id) on delete cascade,
    -- The SDK's `monitor_slug`. Unique per project: that is the identity a job reports under.
    slug       text   not null,

    -- 'crontab' with a five-field expression, or 'interval' with a count and a unit.
    schedule_type  text not null default 'crontab',
    schedule_value text not null,
    schedule_unit  text,
    -- IANA name. Cron expressions are wall-clock, so "0 3 * * *" means 03:00 where the job runs.
    timezone       text not null default 'UTC',
    -- Grace period before a late check-in counts as missed, and the runtime after which a job
    -- that started but never finished counts as timed out.
    checkin_margin_minutes int not null default 5,
    max_runtime_minutes    int not null default 60,

    -- 'ok' | 'error' | 'missed' | 'timeout' — the outcome of the most recent run, or null before
    -- the first one completes.
    status         text,
    last_checkin_at timestamptz,
    -- When the next run is expected. Recomputed on every completed check-in; the sweeper reads it
    -- to find runs that never happened, so it is the load-bearing column here.
    next_due_at    timestamptz,
    -- Set when a miss or timeout has already been reported, and cleared by the next successful
    -- check-in. Without it every sweep of a still-broken job would raise the same alert again.
    reported_at    timestamptz,
    created_at     timestamptz not null default now(),

    constraint monitors_slug_unique unique (project_id, slug),
    constraint monitors_schedule_type_check check (schedule_type in ('crontab', 'interval')),
    constraint monitors_status_check
        check (status is null or status in ('ok', 'error', 'missed', 'timeout'))
);

-- The sweeper's scan: monitors whose next run is overdue.
create index monitors_next_due_idx on monitors (next_due_at) where next_due_at is not null;

-- Individual runs. `check_in_id` lets an SDK open a run ('in_progress') and close it later
-- ('ok'/'error') with its duration, which is what makes max_runtime enforceable.
create table monitor_checkins (
    id           bigserial primary key,
    monitor_id   bigint not null references monitors (id) on delete cascade,
    check_in_id  uuid   not null,
    status       text   not null,
    duration_seconds double precision,
    environment  text,
    release      text,
    created_at   timestamptz not null default now(),
    updated_at   timestamptz not null default now(),

    constraint monitor_checkins_unique unique (monitor_id, check_in_id),
    constraint monitor_checkins_status_check
        check (status in ('in_progress', 'ok', 'error'))
);

create index monitor_checkins_monitor_idx on monitor_checkins (monitor_id, created_at desc);
