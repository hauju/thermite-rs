-- Multiple DSN keys per project, each optionally labeled with the component that
-- reports through it ('saas', 'worker'). One product stays one project — one issue
-- stream, one alert config — while the label is synthesized onto events as a
-- `component` tag, so parts are a filter rather than separate projects.
--
-- Ingest authenticates against this table alone. `projects.public_key` remains as
-- the project's default DSN for display, and is seeded here so it keeps working.
create table project_keys (
    id         bigserial primary key,
    project_id bigint      not null references projects (id) on delete cascade,
    public_key text        not null unique,
    -- Null for the project's original, unlabeled key: events through it get no
    -- component tag, exactly as before this table existed.
    label      text,
    created_at timestamptz not null default now()
);

create index project_keys_project_idx on project_keys (project_id);

insert into project_keys (project_id, public_key)
select id, public_key from projects;
