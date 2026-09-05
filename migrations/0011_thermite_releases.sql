-- Releases, in the order Thermite first saw them.
--
-- The id is the ordering: "newer release" means "first reported later", the same ordering Sentry
-- uses (date_added), because version strings do not sort — git SHAs have no order and 1.10 < 1.9
-- lexically. A release nobody has reported from does not exist yet, and a never-before-seen
-- version is by definition newer than every known one.
create table releases (
    id         bigserial primary key,
    project_id bigint      not null references projects (id) on delete cascade,
    version    text        not null,
    first_seen timestamptz not null default now(),
    constraint releases_version_unique unique (project_id, version)
);

-- "Resolved until the next release": events from this release or older keep the issue resolved
-- (the broken deploy is still out there; that is not news). An event from a strictly newer
-- release — or from no release at all, which cannot be proven old — reopens it as a regression.
-- Null means a plain resolve: any recurrence reopens.
alter table issues
    add column resolved_in_release_id bigint references releases (id);
