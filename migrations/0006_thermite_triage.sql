-- The trigger outbox.
--
-- A row is written in the same transaction as the issue it describes, so a notification can never
-- be lost to a crash between "issue stored" and "agent told about it". Agents drain this queue;
-- one row per *issue*, never per event, so an error storm produces one unit of work rather than
-- thousands.
create table notifications (
    id         bigserial primary key,
    project_id bigint      not null references projects (id) on delete cascade,
    issue_id   bigint      not null references issues (id) on delete cascade,
    -- 'new_issue' the first time a fingerprint is seen, 'regression' when a resolved one returns.
    kind       text        not null,
    created_at timestamptz not null default now(),

    -- Claim lease. A claim that is never acked expires and the row becomes claimable again, so an
    -- agent that dies mid-diagnosis does not strand its work.
    claimed_at timestamptz,
    claimed_by text,
    lease_until timestamptz,

    acked_at   timestamptz,

    constraint notifications_kind_check check (kind in ('new_issue', 'regression'))
);

-- Drives the claim query: only unacked rows are ever of interest, and they are the small minority.
create index notifications_unacked_idx
    on notifications (project_id, created_at)
    where acked_at is null;

-- What an agent concluded about an issue, written back through the API.
--
-- Thermite never calls an LLM itself; these rows are produced by whatever agent drains the queue,
-- which is why `source` records who wrote it.
create table issue_analyses (
    id            bigserial primary key,
    issue_id      bigint      not null references issues (id) on delete cascade,
    -- Free-form agent identifier, e.g. "claude-code" or "ci-triage".
    source        text        not null,
    summary       text        not null,
    details       text,
    suggested_fix text,
    -- 'high' | 'medium' | 'low'. Advisory: a confident-sounding wrong answer is the failure mode
    -- worth surfacing, so the agent is asked to say how sure it is.
    confidence    text,
    -- The revision the analysis was made against, so a reader can tell whether it is still current.
    release       text,
    -- Arbitrary agent-supplied extras: linked PRs, files inspected, tokens spent.
    metadata      jsonb,
    created_at    timestamptz not null default now(),

    constraint issue_analyses_confidence_check
        check (confidence is null or confidence in ('high', 'medium', 'low'))
);

create index issue_analyses_issue_idx on issue_analyses (issue_id, created_at desc);
