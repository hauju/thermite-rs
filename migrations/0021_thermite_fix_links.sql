-- Closing the triage loop: from "here is what broke" to "here is the pull request".
--
-- An agent already receives everything a fix needs — stack frames with source context, the release
-- that crashed, and the last known good release to diff against. What it could not be told is
-- where the code lives, and what it could not hand back was a link to the change it made.

-- Where this project's code lives, so a triaging agent can open a pull request against it.
-- Operator-set through project settings, never client-controlled at ingest: it is the anchor every
-- `fix_url` below is validated against.
--
-- Thermite stores a link and nothing else. It holds no git credential and never calls the host —
-- an error tracker with a write-capable token on the repositories it watches turns every ingest
-- bug into a path to the source.
alter table projects add column repo_url text;

-- The pull request the agent opened for this issue, if it got that far. Null when the analysis is
-- a diagnosis alone, which stays the common case.
--
-- Promoted out of `metadata`, where a link could already be stashed untyped, because this one is
-- rendered as a clickable link on a page an operator trusts. That is exactly why `record()`
-- requires it to share a host with the project's `repo_url`.
alter table issue_analyses add column fix_url text;
