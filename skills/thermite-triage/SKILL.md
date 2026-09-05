---
name: thermite-triage
description: Drive Thermite (self-hosted, Sentry-compatible error tracker) through its MCP tools. Use this whenever the user wants to triage errors, asks "what's broken", "any new errors/crashes", or to diagnose a production issue or exception; after a deploy to check the release went out clean (release health, crash-free rate); to check whether cron/scheduled jobs are running; to look up an error id from application logs; or to set up error tracking for a new app. Also use when running a recurring triage loop. Covers the claim → checkout → diagnose → post_analysis → ack workflow, regression diff ranges, and release-aware resolution.
---

# Thermite triage

Thermite groups application errors into issues and queues each new issue or
regression exactly once for triage. The MCP tools are the whole interface —
everything below is about using them in the right order so no work is lost,
duplicated, or diagnosed against the wrong code.

## The triage loop

1. **`claim_triage`** — not `pending_triage` — to take work. Claiming leases
   the items, so parallel agents (or the next tick of a loop) cannot pick up
   the same issue. Use `pending_triage` only to look without taking. Filter
   with `project`, `level` (e.g. `error`) or `kind` when asked to focus.
2. **Check out the release that crashed.** Each item carries `release`; when
   it is a git SHA, diagnose against that revision, not `main` — the line
   numbers and the bug itself may have moved since. Use a worktree so the
   user's working tree is untouched:
   `git worktree add /tmp/triage-<sha> <sha>` (remove it when done).
3. **For a regression, diff the range first.** `regressed_from_release` is
   the release the fix was verified against — last known good.
   `git diff <regressed_from_release>..<release>` is the change set that
   reintroduced the bug, and the answer is usually in it. A null
   `regressed_from_release` means the issue was resolved without a release
   anchor; fall back to reading the code. For a new issue,
   `first_seen_release` is the upper bound on where the bug was introduced.
4. **`get_issue`** — one call returns the exception chain, every stack frame
   with source context, breadcrumbs, runtime contexts, tag distribution and
   prior analyses. Read `analyses` first: another agent may already have
   diagnosed this; extend or confirm rather than redo. The tag distribution
   is evidence — "all 400 events carry `server_name: web-3`" is a diagnosis
   in itself, and `users_affected` tells you the blast radius.
5. **`post_analysis` before `ack_triage`.** Findings live only in this
   session until posted; acking first and crashing loses the work. Include a
   one-line `summary`, the reasoning in `details`, a concrete
   `suggested_fix`, honest `confidence` (say `low` rather than guessing
   confidently), the `release` you reasoned against so a reader can tell
   whether it is still current, and `fix_url` when you opened a pull request
   (see below).
6. **`ack_triage`** — the item is done and will not be handed out again.
   It is idempotent; re-acking after a retry is safe.

For a recurring loop (`/loop`), claim with a small `limit` and let the
default 900s lease cover an overrunning tick — the lease, not the loop
interval, is what prevents double triage.

## Opening the fix

When the item carries a `repo_url`, you can go past a diagnosis: open a pull
request against that repository and pass the link back as `fix_url` on
`post_analysis`. It renders on the issue as the one action a reviewer sees.

- **You open it, not Thermite.** Thermite holds no git credential and never
  calls the host — it stores the link. Use the repo access you already have.
- **`fix_url` must be on the same host as `repo_url`.** A link on any other
  host is rejected, because the issue page renders it as trusted.
- **Do not resolve the issue when the PR opens.** Resolving anchors the
  release the fix was verified against, so resolving against an unmerged fix
  makes the next crash from the still-broken deploy read as a regression.
  Resolve after it ships, as below.
- **No `repo_url` configured?** Post the diagnosis alone — passing `fix_url`
  then is an error, not a fallback. Tell the user they can set the repository
  in the project's settings.

## After the bug is fixed

`set_issue_status` with `status=resolved` and `in_next_release=true`. The
broken deploy is usually still running and still reporting; a plain resolve
would reopen the issue on the very next event from it. `in_next_release`
keeps it resolved for releases already seen and reopens (as a regression)
only when a *newer* release reports the error — i.e. the fix did not work.
Requires the SDK to send a release; the tool errors if none exists. Use
`ignored` only to suppress an issue permanently.

## Check your own record

`fix_record` grades every fix that was proposed with a `fix_url` against what
production did next — per agent, so `source` on your analyses is your own
scoreboard. A fix is `held` when releases shipped after it and the issue was
seen in none of them, `regressed` when the issue came back in a release that
first appeared after it, and `pending` while nothing has shipped since.

Read it before trusting your own confidence on a similar bug: a `high`
confidence that regressed twice is worth downgrading. Note that it can only
under-credit you — a release that shipped without your merge, and then crashed,
reads as `regressed` — so treat a single loss as a prompt to look, not proof.

## Health checks, not just firefighting

- **`project_stats`** — error rate over time, unresolved/new/regressed
  counts, and dropped events (an SDK that sends nothing and a quota that
  drops everything look identical without it).
- **`release_health`** — crash-free session rate per release, newest first.
  Use it to tell a broken release from a busy one: error counts rise with
  traffic, crash-free rate does not. A null rate means under 50 sessions —
  report "not enough data", not a percentage.
- **`list_monitors`** — cron job health: schedule, last check-in, status
  (`ok`/`error`/`missed`/`timeout`). An empty list means no job is
  instrumented, which is not the same as healthy — say so. A missed or
  overrunning job also becomes an ordinary error issue automatically, so it
  flows through the triage loop like any crash.
- **`get_event`** — when the user pastes an event id from application logs,
  this maps it straight to its issue.

## Setting up a new app

`create_project` returns a DSN; point any unmodified Sentry SDK at it. For
the full agent workflow to work, configure the SDK to send:

- `release` (ideally the git SHA) — enables regression detection, the
  diff-range fields above, and release health;
- session tracking — enables crash-free rates;
- cron check-ins (`monitor_config` on the check-in) — monitors create
  themselves on first sighting, nothing to configure server-side.

An instance's `/llms.txt` describes this surface; the REST API under
`/api/v1` mirrors every tool if MCP is unavailable.

## Reporting back to the user

Lead with what is broken and what you concluded, not the tool calls. Per
issue: title, blast radius (`times_seen`, `users_affected`, environments),
the diagnosis, and the suggested fix. Mention the analysis was posted to
Thermite so the findings are on the issue for whoever opens it next.
