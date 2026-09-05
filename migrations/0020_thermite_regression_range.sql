-- The last known good release for a regression. Reopening a resolved issue clears
-- resolved_in_release_id (see digest), which destroys exactly the fact an agent diagnosing the
-- regression needs: the release the fix was verified against. Digest preserves it here in the
-- same statement, so a triage item can offer `git diff <good>..<bad>` instead of the whole repo.
alter table issues
    add column regressed_from_release_id bigint references releases (id);
