# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Thermite is a self-hosted error tracker that speaks Sentry's wire protocol. Unmodified Sentry SDKs
report into it, events are grouped into issues, and both a Dioxus dashboard and an MCP server read
the same data — so a coding agent can diagnose a bug with the same information a human sees.

Built on Dioxus 0.7 (Rust), PostgreSQL, FerrisKey (OIDC auth), and rmcp. The app compiles into two
binaries via Cargo features: a server (`server`) and a WASM client (`web`).

## Commands

```sh
docker compose up -d          # PostgreSQL + Mailpit          (just init)
just bootstrap                # .env with a fresh SESSION_SECRET
dx serve --addr 0.0.0.0       # dev server, auto-reloads      (just serve)

just check                    # fmt + clippy + tests, what CI runs
just prepare                  # regenerate .sqlx after changing app SQL
```

### CI checks (must pass before merge)

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo machete                                                  # unused dependencies
cargo test --workspace --exclude thermite                      # library crates
cargo test -p thermite --no-default-features --features server # the app crate
```

The app crate needs `--no-default-features --features server` explicitly: its default feature
builds the WASM client, which has no tests and cannot reach a database.

Tailwind must be pre-compiled for clippy/CI, since `dx serve` isn't running:

```sh
bun install --frozen-lockfile
bunx @tailwindcss/cli -i tailwind.css -o assets/tailwind.css
```

## Architecture

### Workspace layout

```
src/                    Dioxus app: pages, components, server modules
crates/thermite-core/   ingest, grouping, digest, and the queries everything reads
crates/thermite-sdk/    the reporting client, for thermite itself and any app that adopts it
migrations/             one sequence for the whole workspace
```

Auth (FerrisKey OIDC, sessions, login UI), crypto and smtp come from
[dx-kit](https://github.com/hauju/dx-kit) as git dependencies pinned by tag
(`auth = { package = "dx-auth", ... }` — renamed so call sites stay `auth::`).
To change them, edit `~/Projects/dx-kit` with a `[patch]` in a gitignored
`.cargo/config.toml` here (see dx-kit's README), then cut and push a new tag.
Thermite's registration allowlist and forwarded-header trust fixes live
upstream in dx-auth as of v0.3.0.

**`thermite-core` knows nothing about sessions, OAuth or rendering.** It exposes two routers and the
application decides how each is exposed:

- `ingest_routes` — authenticates with a DSN public key and must stay reachable by anything on the
  internet. No session, no API key, no CSRF.
- `api_routes` — ships **unauthenticated** from the crate. `src/server/thermite.rs` wraps it in the
  same `ApiAuth` check as the rest of the app. Inverting this would drag sessions and OAuth into the
  ingest crate.

Everything that reads issues — the REST API, the MCP tools, the dashboard's server functions — calls
the same functions in `thermite_core::api::*`. There is no second implementation to drift.

### Feature-gated compilation

Code gated with `#[cfg(feature = "server")]` compiles only for the server; `#[cfg(feature = "web")]`
only for the WASM client. The `web` feature must propagate to sub-crates (e.g. `auth/web`) for their
UI components and CSS classes to be included in Tailwind scanning.

`thermite-core` is a server-only dependency. Anything crossing the server-function boundary needs a
plain serde view model in `src/models/errors.rs` — its `From` conversions are behind `cfg(server)`.

### Ingest

`POST /api/{project_id}/envelope/` and `/store/`. `project_id` must be an integer; SDKs parse it out
of the DSN path. Credentials resolve from `?sentry_key=`, then `X-Sentry-Auth`, then a `dsn` envelope
header — browser SDKs cannot always set request headers. Bodies may be `identity`, `gzip`, `deflate`,
`zstd` or `br`; `Content-Type` is ignored because sentry-rust sends none.

**Ingest is synchronous by design.** It authenticates, parses, groups and writes to Postgres *before*
acknowledging the SDK. Nothing is buffered outside the database, so no accepted event is lost to a
crash, and a slow database backpressures the SDK (which already buffers and retries). Do not
"optimise" this into an ack-then-digest channel without a measured reason — it trades away the
no-loss property.

**Authentication strictly precedes decoding and parsing**, and that ordering is load-bearing. The
`dsn` envelope header is a credential source, but it sits on the envelope's *first line*, so
`dsn_key_from_prefix` decodes only `PREAUTH_PREFIX_BYTES` (8 KiB) to recover it — never the whole
body, and never the item list. Parsing first, as an obvious reading of "the dsn header can only be
consulted after parsing" suggests, lets an unauthenticated request expand a ~20 KB compressed body
into a multi-gigabyte item list for *any* project id, valid or not. For the same reason
`envelope::parse` caps items at `MAX_ITEMS`: the item list is the parser's only unbounded
allocation. A prefix that fails to decode or carries no DSN is a `401`, never a fall-back to
decoding the rest.

Non-event envelope items (transactions, logs, attachments) are **accepted and dropped** with a
`200`. Rejecting an envelope because it contains one would break every real SDK. Three exceptions
are routed to their own subsystems: `check_in` to `thermite_core::monitors` (see "Cron
monitoring"), `session` / `sessions` to `thermite_core::ingest::sessions` (see "Release health"),
and `client_report` to the outcome rollup (see below).

**Every drop is counted.** `thermite_core::ingest::outcomes` records what happened to each request
into the `ingest_outcomes` rollup (`accepted` / `over_quota` / `invalid` / `unsupported:{type}` /
`client_discarded:{reason}`, per project per hour, bucketed on arrival), and the stats API
surfaces the non-accepted outcomes as the "dropped" figures — a drop that leaves no trace is
indistinguishable from an SDK that never sent anything. Counts accumulate per request and flush
once (including on the 429 paths); the pre-body `exhausted()` rejection records its outcome too,
one constant-cost upsert. Sessions deliberately carry no outcome: routine session flushes would
drown the drop counts in noise.

Two of the kinds carry a detail after the colon, because the bare count cannot be acted on — an
SDK flushing transactions thermite does not store is nothing to fix, an SDK whose queue is
overflowing is. Both details come off the wire, so both map through a fixed set with an `other`
catch-all: the outcome is part of this rollup's primary key, and an unbounded key is the
cardinality problem `issue_tags` already had to solve.

**A `client_report` is the SDK's own drop counter** (`protocol::client_report`) — events it threw
away before sending, for its own reasons (`queue_overflow`, `network_error`, `before_send`, a
sample rate). It lands in the same rollup as thermite's own drops: an event discarded inside the
SDK is exactly as invisible as one dropped here, and the reason distinguishes them. Only
categories that would have become events here are counted (`error` / `default` / `security`) — a
discarded transaction is no loss for a store that keeps no transactions. Client reports cost no
quota: an SDK reporting its losses must not be charged for saying so.

Response contract: `200` with `{"id": "<32-hex>"}`; `401` + `X-Sentry-Error` for a bad key (SDKs treat
401 as fatal and stop retrying); `429` + `Retry-After` + `X-Sentry-Rate-Limits` when over quota.

**The per-project quota is charged per event item, not per request.** An envelope may carry many
events, so charging per request let one request store hundreds for the price of one — and each of
those mints permanent `issues`/`notifications` rows. Hitting the limit mid-envelope returns `429`
with the events already digested left committed (each digest is its own transaction, and an
accepted event is durable); the SDK retries and its event ids deduplicate. A separate
`exhausted()` pre-check rejects an already-over-quota project *before* the body is decoded, and
deliberately charges nothing — the accounting belongs to the per-event `check()`.

### OTLP logs

`POST /api/{project_id}/otlp/v1/logs` (`thermite_core::ingest::otlp`), for services already
instrumented with OpenTelemetry that will not adopt a second SDK. Same DSN credential, same
per-project quota, same per-IP limiter as the envelope endpoints — `is_ingest_path` has to list it
or the global backstop answers instead, with a plain-text 429 an exporter cannot read.

**A second reader, not a second pipeline.** A record is converted to the Sentry-shaped payload
`digest()` already takes, and nothing past `ingest::otlp::convert` knows an event arrived over
OTLP. Grouping, the rollups, retention, alerting and triage are all the code that was already
there.

Four things to preserve:

- **Only `ERROR` (severity 17) and above becomes an event.** A collector forwards an application's
  whole log stream; the floor is the difference between error tracking and a log sink with one
  issue per line. Dropped records are counted as `unsupported:log` — not reported back as
  `partial_success.rejected_log_records`, which would make every export log a warning.
  A record carrying `exception.type` is an error whatever its severity says.
- **Both encodings are read, by two hand-written readers.** OTLP/JSON is not mechanical proto3
  JSON: camelCase, 64-bit integers as strings, `AnyValue` as a one-key object, and hex trace ids.
  `equivalent_encodings_produce_the_same_records` is what stops the two drifting.
- **The record cap bounds the decode, not the result.** An empty `LogRecord` is two bytes on the
  wire against ~150 in memory, so deriving the whole message would be a 75x amplifier — the same
  problem `envelope::parse` caps with `MAX_ITEMS`. That is why the container messages are walked
  by hand over prost's primitives instead of derived.
- **Attributes become `extra`, never tags.** They are client-controlled and unbounded, and
  `issue_tags` outlives the events it summarises. The one exception is `service.name`, which
  becomes the `component` tag so one collector can fan many services into one project.

What OTel gives up is stack frames: `exception.stacktrace` is one language-specific string with no
structure to recover, so it is kept verbatim in `extra` and the issue's culprit stays empty.

### Grouping

Ported from Bugsink's `bugsink-v2`. An explicit `fingerprint` wins (with `{{ default }}` substituted,
not taken literally); otherwise the key is the exception type plus its **normalized** value, or the
log message. The last exception in a chain is the one used. Note v2 deliberately dropped `transaction`
from the key — v1 split one bug across every route that could trigger it.

`protocol/normalize.rs` ports Sentry's `parameterization.py` so `10.0.0.5` and `10.0.0.9` group
together. Two upstream deficiencies are fixed and marked in comments: the `duration` pattern matched
a single digit before `s` (so `30s` and `45s` grouped apart), and the `ip` alternatives only partially
consumed compressed IPv6. Do not "restore fidelity" to upstream there.

### Triage and agents

Thermite never calls a model. `digest()` writes a `notifications` row **in the same transaction** as
the issue — the outbox pattern, so there is no window where an issue exists but nothing knows to look
at it. One row per *issue*, never per event, so an error storm is one unit of work.

Agents claim work with a **lease**, not a flag: a `/loop` firing every 10 minutes while a triage run
takes 12 would otherwise re-diagnose the same issue. An agent that dies releases its work when the
lease expires. Findings come back through `post_analysis` and appear on the issue detail page.

**Triage does not have to stop at a diagnosis.** A project can declare a `repo_url`
(project settings), which rides along on the triage item and `get_issue`; an agent that opens a
pull request hands the link back as `fix_url` on `post_analysis`, and the issue page renders it as
the reviewer's one action. Three things there are load-bearing:

- **Thermite holds no git credential and never calls the host.** It stores a link. An error tracker
  with a write-capable token on the repositories it watches turns every ingest bug into a path to
  the source.
- **`fix_url` must share a host with the project's `repo_url`** (`analyses::check_fix_url`), and a
  project without one cannot accept a fix link at all. The value comes from whatever agent drained
  the queue and is rendered as a trusted link on the issue page. Host rather than full prefix: a
  pull request from a fork is legitimate, and over-tightening only pushes agents to stash the link
  in `metadata`, where nothing checks it.
- **A posted fix does not resolve the issue**, and the tool descriptions say so. Resolving anchors
  `resolved_in_release_id`; doing it against an unmerged fix makes the next event from the
  still-broken deploy read as a regression.

**A person can answer back.** The issue page has a note form; a note is stored as an analysis
with `metadata.kind = "note"` and the user's name as `source`, so an agent reading `get_issue`
sees "not the cache, the retry loop" beside the machine findings. The dashboard renders notes on
the secondary hue and without the confidence or fix affordances.

**A proposed fix is then graded against production** (`api::fixes`), which is the half that
makes the loop worth closing: `pending` while no release has shipped since the fix, `held` when
releases shipped and the issue was in none of them, `regressed` when it came back in a release
first seen after the fix. Per `source`, so it is a per-agent hold rate — an eval signal generated
by production rather than by a benchmark. Three properties to preserve:

- **The verdict is derived on read, never stored.** It needs no ingest-time bookkeeping: `releases`
  is already ordered by first sighting and `issue_tags` already records which releases an issue was
  seen in. A stored verdict would be a fourth rollup to maintain and to invalidate.
- **`pending` is a state, and `hold_rate` is null until something settles.** Scoring an
  undeployed fix as a win is the failure mode this exists to avoid, and a rate over zero samples
  reads as a fact while measuring nothing.
- **It can under-credit but never over-credit.** A release that ships *without* the merge and still
  crashes reads as `regressed`. The alternative — trusting that the next release contains the fix —
  would report fixes as holding while the bug is live, which is the one error that makes a
  scoreboard worse than no scoreboard.

Triage items carry the `release` the error came from. When that is a git SHA, an agent can check out
the revision that actually crashed — diagnosing against the wrong revision produces confident but
wrong answers.

Regressions additionally carry the **regression range**: reopening moves the issue's
`resolved_in_release_id` anchor to `regressed_from_release_id` instead of just clearing it, so the
release the fix was verified against survives as the last known good. Triage items and the issue
detail surface it as `regressed_from_release` (plus `first_seen_release` from the oldest retained
event) — `git diff <good>..<bad>` beats reading the whole repo.

Resolution is release-aware: resolving with `in_next_release` anchors on the newest row in
`releases`, and digest reopens (and queues a regression) only for a release first seen *after* that
anchor, or an event with no release. Releases are ordered by first sighting (`releases.id`), never
by parsing version strings — git SHAs have no order and `1.10` sorts below `1.9`.

### Cron monitoring

`thermite_core::monitors` implements Sentry's check-in protocol: jobs send `check_in` envelope
items to the same ingest endpoint with the same DSN credential, and the monitor is created on
first sighting from the `monitor_config` the SDK carries — nothing is configured twice.

**A missed or overrunning run becomes an ordinary error event**, digested through `digest()` like
any exception. Grouping, the outbox, alert delivery, triage and retention then apply unchanged;
cron monitoring is a new *source* of events, not a second pipeline. The synthetic event carries an
explicit fingerprint (`monitor:{slug}:{missed|timeout}`), so a job broken for a week is one issue,
and a timeout stays distinct from a miss — they point at different bugs.

Four things to preserve:

- **An `in_progress` check-in must not advance `next_due_at`.** It says the job started, not that
  it finished; moving the window would make a job that hangs forever look punctual.
- **`reported_at` is set in the same statement that selects an overdue monitor** (`for update skip
  locked`), so concurrent replicas cannot both report one miss. A successful check-in clears it,
  which is what re-arms the alert for the next failure.
- **The sweep advances `next_due_at` past a reported failure**, so the *next* missed run is
  reported too rather than the monitor going quiet after one alert.
- **A check-in with no parseable schedule that names no existing monitor creates nothing.** With
  no schedule there is nothing to be late for, so the row could never alert — it would be junk
  that looks like coverage.

Crontab expressions are evaluated in the monitor's timezone (`chrono-tz`), because cron is
wall-clock: `0 3 * * *` in Berlin is not 03:00 UTC. An unknown timezone falls back to UTC rather
than failing — a typo should shift the window, not silence the monitor.

### Release health

`session` and `sessions` envelope items become counters in `session_counts`
(`thermite_core::ingest::sessions`), never rows. They are the **denominator**: an error count rises
with traffic, so it cannot tell a broken release from a busy one — crash-free rate can.

Four things to preserve:

- **The counting rule** (`protocol::session`) counts only updates an SDK sends *exactly once* per
  session: `init` for the total, and the terminal status for how it ended. A session is reported
  several times as it progresses, so counting every update multiplies the totals — and a row per
  `sid` to deduplicate would be unbounded storage for a number only ever read as a rate.
- **Buckets key on the session's `started`, never on the update that closed it.** A session
  beginning at 10:59 and crashing at 11:01 must land its total and its crash in one bucket, or that
  bucket reports a crash rate above 100%.
- **A session with no release is dropped.** The rollup is keyed on `releases.id`, whose per-project
  cap (`MAX_RELEASES_PER_PROJECT`) is the only thing bounding this table's cardinality.
- **Sessions cost no quota.** The quota is charged per *event* item; making a routine session flush
  eat the error budget would silence the errors it exists to contextualise.

The crash-free rate is `None` below 50 sessions and renders as "not enough data" — one crash in
three sessions is 66.7%, which looks like a catastrophe and measures nothing. There is deliberately
no `environment` dimension (client-controlled and unbounded) and no crash-free *users* (it needs a
distinct-user set, which would put identifiers in a rollup that outlives event retention — the
problem `issue_tags` already had to solve).

### Alerting

`src/server/alerts.rs` is the outbox's *second* consumer: it claims rows via
`thermite_core::alerts::claim` every minute and delivers to `THERMITE_ALERT_EMAIL` /
`THERMITE_ALERT_WEBHOOK`, unless the project carries its own routing (`projects.alert_email` /
`alert_webhook` — an override replaces the global recipients for that project, it does not add to
them). The loop always spawns, because project routing can appear at runtime; while *nothing* is
configured anywhere, the backlog floor rides along with the clock so enabling alerting later
starts clean. Invariants:

- **The alert columns are independent of the triage lease/ack columns.** An agent acking its work
  says nothing about whether a human was told; do not "simplify" the two consumers into one flag.
- **At-least-once, per channel**: a row is settled only after every configured channel succeeded,
  but each channel's success (`alert_email_at` / `alert_webhook_at`) is recorded separately, so a
  failing sibling does not re-spam the healthy channel every minute. A duplicate is acceptable; a
  silently dropped alert is not.
- **Rows claim a lease** (`alert_lease_until`, the triage pattern) so N replicas deliver each
  alert once, not N times; failures back off exponentially and dead-letter after 10 attempts with
  a loud log line, instead of a poison row blocking the queue head forever. SMTP sends are capped
  at 30s so a hung host cannot silently stop all alerting.
- **Eligibility starts at the durable backlog floor** (`alert_state.backlog_floor`, recorded when
  alerting is first enabled) — not a rolling window. Enabling alerting on an old instance must not
  flood the recipient, but an outage longer than any fixed window must lose nothing.

### MCP

`POST /mcp` (`src/server/mcp.rs`) is an **rmcp 3.0** `StreamableHttpService`. It negotiates every
protocol version from `2024-11-05` through `2026-07-28`; under `2026-07-28` (SEP-2567) the core is
stateless and requests carry `Mcp-Method`, plus `Mcp-Name` for `tools/call`.

Two auth layers: `mcp_auth_challenge` does a presence-only check at the edge (no credential → `401` +
`WWW-Authenticate` pointing at the protected-resource metadata, which triggers OAuth discovery); each
tool then does the real `api_auth` validation. Both connectors work — `oat_` API keys for Claude Code,
the OAuth flow for claude.ai.

Three things to preserve when editing:

- `#[tool_handler(router = self.tool_router)]` — the attribute otherwise defaults to
  `Self::tool_router()`, which **rebuilds the router and re-derives every tool's JSON schema on every
  `tools/list` and `tools/call`**. rmcp's own examples get this wrong.
- Do not hand-write `initialize`. Its default negotiates the protocol version against what the client
  asked for; pinning it breaks older clients and changes whether a request is served statelessly.
- Tool-level failures return `Ok(CallToolResult::error(...))`, not `Err`. The request routed fine and
  the tool ran, so the agent should see the message and correct itself. Only auth failures and
  infrastructure faults are protocol errors (`present()` in `mcp.rs` encodes this).

`allowed_hosts` is derived from `BASE_URL`: rmcp validates `Host` to block DNS rebinding, and its
default allowlist is loopback-only, which would reject every request to a deployed instance.

`GET /llms.txt` (`src/server/llms.rs`) is the unauthenticated discovery page for agents: what
Thermite is, where `/mcp` lives, how to authenticate, and the triage loop. It describes the
interface, never data — keep it that way.

### Self-reporting

Thermite watches itself with `crates/thermite-sdk`, wired in `main.rs::init_self_reporting` and
active whenever `THERMITE_DSN` is set. **Point it at a different thermite, or at least a different
project** — an instance reporting its own ingest failures into the ingest that is failing loses
exactly the events worth having.

`thermite-sdk` writes the same envelopes `thermite-core` reads, but shares no code with it: core
reads them and drags in sqlx and axum to do it, the SDK writes them and has to build for
`wasm32-unknown-unknown`. What keeps the two in step is a pair of tests in `server/thermite_tests.rs`
that send the same message through this SDK *and* an unmodified Sentry client and assert one issue
with `times_seen` 2 — not a shared type. Unmodified Sentry SDKs remain first-class; this exists
because seventeen `sentry-*` crates is a lot of machinery to fill in about fourteen fields, and
none of it works under wasm.

Three things to preserve:

- **The `tracing` layer never reports its own crate's records.** The transport logs a failed
  delivery at `ERROR`; reporting that sends another envelope, which fails, which logs — a loop with
  no bound that spins fastest exactly when thermite is unreachable.
- **`THERMITE_RELEASE` should be the deployed git SHA.** Triage hands an agent the release an error
  came from and expects to `git diff` against it; the `thermite@<version>` fallback never changes
  and names no revision.
- **CI does not build the SDK for wasm on the host jobs**, so the `web` feature has its own job
  (`wasm client`). The host build cannot catch its breakage: `uuid` needs `uuid/js` to compile at
  all under `wasm32-unknown-unknown`, and `keepalive` is set through `Reflect` because web-sys 0.3
  does not type it on `RequestInit`.

### Dashboard

`/dashboard` (all projects with attention flags — new issues, failing cron monitors, dead-lettered
alerts — read from `api::overview`, counters and rollups only), `/projects`, `/projects/{slug}`,
`/projects/{slug}/settings` (rename, alert routing, component keys, deletion — configuration lives
here, the projects list only shows what you copy into an SDK), `/issues/{id}`. Server functions in
`src/errors_data.rs` call `thermite-core` directly rather than round-tripping through `/api/v1`.

**One project can be public.** `THERMITE_DEMO_PROJECT` names a slug anyone may read without a
session: `errors_data.rs` decides per request (`reader` / `allow_read`) and every read that can
serve a visitor checks the project it is about to return — issue- and event-id reads resolve the
project first. `get_project` withholds the DSN, component keys and alert routing from a visitor;
writes keep requiring a session, and the pages hide the controls. The MCP and REST surfaces are
untouched. The rule is a pure function with tests; keep it that way. While the flag is set,
`server::demo_feed` raises a playground event into that project every 10 to 30 minutes at the
release it last saw, so the demo never goes flat and never reads as a new deploy.

Pages use `use_resource` rather than `use_server_future`, deliberately: these are authenticated views
behind skeletons, and blocking SSR on database queries buys nothing. There is no hydration mismatch
because both server and client start from `None`.

The issue page links releases and in-app frames into the project's `repo_url`
(`src/models/repo_links.rs`: commit, compare and blob URLs per forge), and only when the release
is a git SHA — a forge can show a file at a revision, not at `1.4.2`, and a link to `main` would
point at code that has moved since the crash.

Charts are inline SVG (`src/components/sparkline.rs`) — bars over a fixed bucket count with no axes
or interaction, where a charting dependency would be more code than the shapes. A non-zero bucket
always gets a visible sliver so "rare" never renders identically to "never".

### Key patterns

- **Global state**: `AppState::global()` via `OnceLock`, also an Axum extractor. Carries a
  `thermite_core::ThermiteState` built from the same pool.
- **Router assembly**: every route and layer lives in `src/server/router.rs` so tests drive the same
  stack the binary does. Ordering matters — rate limiting outside the extensions it reads, security
  headers outside the handlers whose errors they must cover.
- **Server functions**: `#[post("/api/...")]` with an optional `session: auth::UserSession`
  parameter. `UserSession::data()` takes ownership, so pass the session by value.
- **Axum route params**: curly braces `"/api/{id}"`, not `"/api/:id"` — colons panic at runtime in
  Axum 0.8+.
- **Migrations are immutable once applied**: sqlx records a checksum, so editing an applied file —
  even a comment — fails the next boot. Add a new numbered migration instead.
- **Dashboard numbers read the `event_counts` rollup**, never `count(*)` over `events`. Counting rows
  works at four events and dies at four million, and counters cannot be rebuilt once old events are
  dropped — which retention does routinely, so this is not hypothetical.
- **Tag filters and distributions read the `issue_tags` rollup**, maintained at ingest for the same
  reason. `environment`, `release`, `server_name` and `transaction` are synthesized into it, so the
  environment filter is just a tag filter. So is `component`: a project can mint additional
  labeled DSN keys (`project_keys`), one per part of a product ('worker', 'saas'), and the label
  is stamped onto everything ingested through that key. One product stays one project — one
  issue stream, one alert config, cross-part bugs group into one issue — with parts as a filter,
  where Sentry would force a second project. Ingest authenticates against `project_keys` alone
  (`projects.public_key` is the seeded, unlabeled default); an SDK-sent `component` tag wins
  over the key's label. Its cardinality is bounded at write time — at most 1000
  distinct values per (issue, key), the `user` tag is never synthesized from an IP address, and
  releases cap at 10k per project — because every value is client-controlled and the rollup
  outlives the events. `users_affected` reads a counter on `issues` maintained by `digest()`,
  never `count(*)` over this table.

### Retention

`thermite_core::retention` drops events by age *and* a per-project cap, whichever bites first; the
sweep also ages out all four rollups (`event_counts`, `issue_tags`, `session_counts`,
`ingest_outcomes`). The sweeper is spawned hourly in `router::build`. Four properties to preserve
when editing:

- **Age keys on `received_at`, never `timestamp`.** `timestamp` is client-supplied, so keying on it
  lets an SDK evict its own events by sending 1971 or make them immortal by sending 2099.
- **Issues and analyses survive everything; rollup rows outlive their events but age out with the
  policy.** Counts are maintained during ingest rather than computed from `events` because they
  cannot be rebuilt once events drop. But the rollups grow with *cardinality*, not time — one row
  per distinct tag value, per hourly bucket — so "rollups are forever" would mean retention never
  bounds disk, and `issue_tags` carries user identifiers that `THERMITE_RETENTION_DAYS` must
  genuinely erase. The sweep prunes rollup rows older than the age policy; the permanent history
  lives on the issue row (`times_seen`, `users_affected`).
- **Deletes are batched.** One statement over millions of rows holds locks long enough to stall
  ingest, which is the one thing retention must not do.
- **Unacked notifications are never dropped.** An unacked row is work nobody did; discarding it on
  age would hide that an issue was never looked at.

### sqlx: two styles, on purpose

The **app crate** uses the compile-time-checked `query!` macros with committed `.sqlx` metadata and
`SQLX_OFFLINE=true` in `.cargo/config.toml`, so a fresh clone builds with no database. After changing
any SQL there, run `just prepare` against a live database.

**`thermite-core` deliberately does not.** Its SQL is assembled from shared column-list constants
(`ISSUE_COLUMNS`, `EVENT_COLUMNS`); the macros only accept literal strings, so converting would mean
inlining those constants at a dozen call sites — trading one source of truth for a dozen copies. Its
queries are covered by integration tests instead.

Note what the offline cache actually catches: entries are keyed by a hash of the SQL string, so
changing a *query* without re-preparing fails the next offline build, but changing the *schema* while
leaving queries untouched leaves stale entries that still compile and fail at runtime. The `schema`
CI job covers that second case by building against a real database.

Two gotchas: `tower-sessions-sqlx-store` forces sqlx's `time` feature on, so the macros map
`TIMESTAMPTZ` to `time::OffsetDateTime` unless reads are annotated `col as "col: Ts"`; and timestamps
are therefore written by the database (`NOW()`) rather than bound from Rust, which also keeps the
database clock authoritative across replicas.

### Tests

- **`crates/thermite-core/tests/`** — protocol, grouping, ingest, stats and triage, driving
  thermite-core's routers directly. These need `#[sqlx::test(migrations = "../../migrations")]`
  because the migrations live at the workspace root, not beside the crate.
- **In-crate `#[cfg(test)]` modules** in the app — the app is a binary crate, so `tests/` cannot
  import it. `src/server/thermite.rs` covers the ingest/read-API auth split and drives a real
  `sentry` client against a loopback listener; `src/server/router.rs` covers the middleware stack,
  OAuth and MCP.
- Hand-built requests to `/mcp` need a `Host` header (rmcp's DNS-rebinding check) and, on
  `2026-07-28`, `Mcp-Method` / `Mcp-Name`.

### Infrastructure

- **PostgreSQL** on host port 5433: application data, the session store (`tower-sessions-sqlx-store`, own
  schema, hourly pruning), shared rate-limit counters, and all error data.
- **Three connection pools** (`src/server/db.rs`): interactive (dashboard/sessions/API/MCP),
  ingest, and background (sweepers). Synchronous ingest holds a connection per event, so an error
  storm must saturate its own pool — not the one logins and `/ready` depend on. `/health` is
  liveness-only *because* the Docker `HEALTHCHECK` restarts on it; a probe that touched the
  database would restart the container mid-storm, which is why the DB round-trip lives on
  `/ready` instead. Do not merge the pools or "simplify" the two probes into one.
- **Mailpit**: local SMTP. SMTP 1028, web UI 8028.

Both are published on `127.0.0.1` at offset host ports so the stack coexists with other projects'
Postgres/Mailpit containers and a default-credential dev database is never exposed to the network.

### Docker

The `Dockerfile` compiles nothing — it packages a bundle built outside Docker. CI runs
`dx bundle --web --release` and the image copies `target/dx/thermite/release/web` into a slim Debian
runtime. Docs, `build.rs` output and SQL migrations are embedded in the server binary, so none ship
as files. The app listens on 8080 with a `HEALTHCHECK` against `/health`. Building the image locally
requires running `dx bundle --web --release` first.

### Environment variables

Copy `.env.example` to `.env`. Key variables: `DATABASE_URL`, `BASE_URL`, `SESSION_SECRET` (hex, 64+
bytes), the `FERRISKEY_*` set, SMTP settings, optional `THERMITE_MAX_ENVELOPE_BYTES` /
`THERMITE_RATE_LIMIT_PER_MINUTE`, optional `THERMITE_DSN` + `THERMITE_RELEASE` + `ENVIRONMENT`
for self-reporting (see "Self-reporting" below).

### Styling

TailwindCSS 4 + DaisyUI 5, dark theme default. Source scanning is configured in `tailwind.css`;
Dioxus 0.7+ auto-detects it and runs Tailwind during `dx serve`. Icons via `dioxus-free-icons`
(Lucide). `safelist-docs-kit.html` exists because Tailwind cannot scan classes inside `~/.cargo`.

The palette is molten — orange primary, gold secondary, warm charcoal base — after the material
the product is named for. Two things there are load-bearing rather than taste:

- **The warm hues are spaced on purpose**: error ~18° (rose-red) → primary ~47° (orange) →
  warning ~78° (gold). This is an error tracker, so `btn-primary` sits beside `badge-error` and
  `fill-error` on nearly every screen; `error` was moved off its old washed brick red precisely
  so it stops reading as a dimmer primary. `accent` stays cyan as the only cool hue. Closing
  those gaps makes primary buttons look like danger buttons.
- **Light-mode primary is a darker orange than dark-mode's.** DaisyUI spends one token on both
  button fills and `text-primary` link ink, and the vivid orange fails contrast as ink on a
  near-white page.
- **Light-mode error and warning are darker still, and that is about lightness, not hue.** Hue
  spacing only helps a reader who can see hue; darkening primary for the point above left light
  primary and error 1.03:1 apart in *luminance*, and a deuteranopia simulation collapsed the
  danger zone's Save and Delete into one olive swatch. The current values keep ~1.5:1 between
  them. Check luminance, not just hue, before touching either.

Two DaisyUI 5 traps this palette walks into, both silent: `badge-ghost` and a bare `btn` resolve
to `base-200`, which is exactly the card surface every page uses — they render at 1.00:1, i.e.
invisible. Use `badge-neutral` and `btn-outline` on a card. And `form-control` / `label-text` /
`input-bordered` no longer exist; they are no-ops that happen to lay out correctly today.

The mark (`src/components/logo.rs`, mirrored in `assets/mark.svg`) is a molten triangle around a
white-hot core — the reaction is hottest at its centre. `assets/favicon.svg` and `assets/pwa/*.svg`
are the icon sources; `just icons` re-renders the `.ico` and `.png`s from them, so the committed
binaries are never the only copy.

### Removed

**Polar billing is gone.** It was inert — no `POLAR_*` value in any environment, no checkout or
pricing route, and its single `require_active` gate protected `premium_ping`, a demo endpoint
returning "pong — premium access confirmed". Removed as the focused change this section previously
called for, against a green suite.

`models::subscription::SubscriptionInfo` and the `users.subscription` JSONB column stay: the column
exists in deployed databases behind a migration, and `src/server/user.rs` keeps a `#[sqlx::test]`
asserting the JSONB → `Json<SubscriptionInfo>` round-trip, which is easy to get wrong. Nothing
writes it now — wiring billing back means adding a writer, not a schema change.
`templates/dx-saas-template` still carries the reference wiring.

`/pricing` exists as a **marketing page only** — the tiers it advertises (1k / 100k / 1M events per
month) are not enforced anywhere, and its calls to action go to signup, not a checkout. The quota
machinery those numbers describe is real (`ingest::ratelimit`, `THERMITE_MAX_EVENTS_PER_PROJECT`),
but nothing maps a plan onto it yet. Wiring billing back means a writer for `users.subscription`, a
plan → quota mapping, and a checkout — in that order.

---

## Dioxus 0.7 Reference

You are an expert [0.7 Dioxus](https://dioxuslabs.com/learn/0.7) assistant. Dioxus 0.7 changes every
api in dioxus. Only use this up to date documentation. `cx`, `Scope`, and `use_state` are gone.

### Launching

```rust
use dioxus::prelude::*;

fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    rsx! { "Hello, Dioxus!" }
}
```

### RSX

```rust
rsx! {
    div {
        class: "container",
        color: "red",
        width: if condition { "100%" },
        "Hello, Dioxus!"
    }
    for i in 0..5 {
        div { "{i}" }
    }
    if condition {
        div { "Condition is true!" }
    }
    {children}
    {(0..5).map(|i| rsx! { span { "Item {i}" } })}
}
```

### Assets

```rust
rsx! {
    img { src: asset!("/assets/image.png"), alt: "An image" }
    document::Stylesheet { href: asset!("/assets/styles.css") }
}
```

### Components & Props

- Annotate with `#[component]`, function name starts with capital letter.
- Props must be owned (`String` not `&str`), implement `PartialEq` + `Clone`.
- Wrap in `ReadOnlySignal` for reactive props.
- Re-renders when props change or internal reactive state updates.

### State

```rust
// Local state
let mut count = use_signal(|| 0);
let doubled = use_memo(move || count() * 2);

// Read: count() clones, count.read() borrows
// Write: *count.write() += 1  or  count.with_mut(|c| *c += 1)

// Context API
use_context_provider(|| Signal::new(value));
let ctx = use_context::<Signal<T>>();
```

### Async

```rust
let data = use_resource(move || async move { fetch().await });
match data() {
    Some(value) => rsx! { "{value}" },
    None => rsx! { "Loading..." },
}
```

Reading a resource inside `match` needs `&*data.read_unchecked()` when the arms borrow from it.

### Routing

```rust
#[derive(Routable, Clone, PartialEq)]
enum Route {
    #[layout(NavBar)]
        #[route("/")]
        Home {},
        #[route("/blog/:id")]
        BlogPost { id: i32 },
}
```

### Server Functions

```rust
#[post("/api/double/:path/&query")]
async fn double_server(number: i32, path: String, query: i32) -> Result<i32, ServerFnError> {
    Ok(number * 2)
}
```

Server functions with `#[get]` cannot have body parameters beyond state/session — use `#[post]` when
parameters are needed. Use `#[get("/path")]` only for plain endpoints with no params (like health
checks or llms.txt).

Route components must be imported with `use` in the file where the `Route` enum is defined
(`routes.rs`), so the Routable derive macro can find them.

### Hydration

- Prefer `use_server_future` for data that should be server-rendered; `use_resource` is correct when
  a skeleton is acceptable and you do not want SSR blocked on I/O.
- Browser-only APIs (e.g. `localStorage`) must go in `use_effect`.
