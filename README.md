# Thermite

[![Licence: AGPL-3.0](https://img.shields.io/badge/licence-AGPL--3.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-2024_edition-orange.svg)](rust-toolchain.toml)

Self-hosted, agent-native error tracking, written in Rust. Thermite speaks Sentry's wire
protocol — point any unmodified Sentry SDK at it and errors group into issues in your own
Postgres. Your coding agent triages them over MCP and leaves its diagnosis on the issue page.

A hosted instance runs at **[thermite.rs](https://thermite.rs)**.

Thermite is built for reading by an agent, not just by a human. `GET /api/v1/issues/{id}` returns
the issue *and* its latest event in full — exception chain, every stack frame with source context,
breadcrumbs, contexts — so a coding agent can diagnose a bug in one request instead of walking a
resource tree. An `rmcp` MCP server exposes exactly these calls as tools, so Claude Code and claude.ai
connectors both work against the same contract.

## Layout

```
thermite/
  src/                    Dioxus 0.7 app: pages, docs, auth, OAuth-for-MCP, /mcp
  crates/thermite-core/   ingest, grouping, digest, and the queries everything reads
  migrations/             one database, one migration sequence
```

Auth (FerrisKey OIDC, sessions, login UI), crypto and smtp are shared
[dx-kit](https://github.com/hauju/dx-kit) crates, pinned by git tag.

`thermite-core` knows nothing about sessions, OAuth or rendering. It exposes two routers and the
application decides how they are exposed:

- **Ingest** authenticates with a DSN public key and must stay reachable by anything on the
  internet. It carries no session and no API key.
- **The read and triage API** ships *unauthenticated* from the crate; the app wraps it in the same
  `oat_` API key / FerrisKey JWT check as everything else. Inverting that would drag sessions and
  OAuth into the ingest crate.

## Stack

**Dioxus · axum · Postgres · sqlx.** No Redis/Valkey, no message broker, no worker pool.

Bugsink's central claim is that a queue and broker are unnecessary for this workload; in Rust the
margin is wider still. Ingest authenticates, parses, groups and writes to Postgres *before*
acknowledging the SDK, which means no accepted event can be lost to a crash or restart, and a slow
database applies backpressure to the SDK (which already buffers and retries on its own). Rate-limit
counters live in-process — a network hop per event to enforce an approximate quota is pure cost.

Valkey earns its place when ingest runs on several nodes and quota state has to be shared. Not
before.

## Quickstart

```bash
docker compose up -d          # Postgres + Mailpit
cp .env.example .env          # DATABASE_URL, BASE_URL, SESSION_SECRET, FerrisKey
dx serve                      # the app: dashboard, docs, ingest, /api/v1, /mcp
```

Create an API key in Settings, then a project:

```bash
curl -X POST localhost:8080/api/v1/projects \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"slug":"my-app","name":"My App"}'
# → { "id": 1, "slug": "my-app", "dsn": "http://<key>@localhost:8080/1" }
```

Point an SDK at the printed DSN and you are collecting:

```rust
let _guard = sentry::init("http://<key>@localhost:8080/1");
sentry::capture_message("it broke", sentry::Level::Error);
```

Then read it back:

```bash
curl -H "Authorization: Bearer $TOKEN" localhost:8080/api/v1/projects/my-app/issues
curl -H "Authorization: Bearer $TOKEN" localhost:8080/api/v1/issues/1
```

## Ingest API (Sentry-compatible)

| Endpoint | Notes |
|---|---|
| `POST /api/{project_id}/envelope/` | The modern endpoint. `project_id` must be an integer — SDKs parse it out of the DSN path. |
| `POST /api/{project_id}/store/` | A bare event payload, for older SDKs. |

Credentials are resolved from `?sentry_key=`, then `X-Sentry-Auth`, then a `dsn` envelope header —
in that order, because browser SDKs cannot always set request headers. Request bodies may be
`identity`, `gzip`, `deflate`, `zstd` or `br`; `Content-Type` is ignored, since sentry-rust sends
none.

Success is `200` with `{"id": "<32-hex event id>"}`. A bad key is `401` with `X-Sentry-Error` (SDKs
treat 401 as fatal and stop retrying, and surface that header in their debug logs). Over quota is
`429` with `Retry-After` and `X-Sentry-Rate-Limits`.

## Read API

All endpoints require `Authorization: Bearer <token>`.

| Endpoint | Returns |
|---|---|
| `GET /api/v1/projects` | Projects with their DSN, unresolved issue count, 24h event count |
| `GET /api/v1/projects/{slug}/issues` | `?status=` `?q=` `?environment=` `?tag=key:value` `?sort=last_seen\|events` `?limit=` `?offset=`. Each row carries a 24h sparkline |
| `GET /api/v1/projects/{slug}/stats` | `?window=24h\|7d\|30d`. Rate over time + current state |
| `GET /api/v1/projects/{slug}/tags/{key}` | The values seen for one tag key, with counts — `…/tags/environment` feeds a dropdown |
| `GET /api/v1/issues/{id}` | Issue **plus its latest event in full**, and any prior agent analyses |
| `GET /api/v1/issues/{id}/events` | Recent events for an issue |
| `GET /api/v1/events/{event_id}` | One event, by the id the SDK assigned it |
| `GET /api/v1/triage/pending` | What is waiting for triage. Read-only |
| `POST /api/v1/triage/claim` | Atomically take work, with a lease |
| `POST /api/v1/triage/{id}/ack` | Done. Idempotent |
| `POST /api/v1/triage/{id}/release` | Give the work back without acking |
| `POST /api/v1/issues/{id}/status` | Resolve, ignore or reopen |
| `GET`/`POST /api/v1/issues/{id}/analyses` | Read / write agent findings |

`exception` and `breadcrumbs` are always returned as plain arrays. SDKs send these three different
ways (`{"values": [...]}`, a bare list, a bare object); normalizing here means a caller never has to
branch. Everything an SDK sent that Thermite does not promote to a column is preserved and served
back from the stored payload.

## Dashboard numbers

Rate charts, sparklines and "events today" all read the `event_counts` rollup — one row per issue,
hour and level, incremented inside the digest transaction. Counting rows in `events` works fine at
four events and falls over at four million, and counters can't be rebuilt once old events are
dropped, so the rollup has to exist before there's data worth charting.

```
GET /api/v1/projects/demo/stats?window=24h

window=24h resolution=1h
events=38  unresolved=2  new=2  regressions=0
events/hour: ▁▁▁▂▁▁▁▂▁▁▁▂▁▁▁▂▁█▁▂▁▁▁▂   (peak 9)
```

Three things it guarantees:

- **The series is continuous and zero-filled**, so a chart never has to reason about gaps.
- **The headline total is summed from the series**, not queried separately — the number and the
  chart can't disagree.
- **Retries don't inflate it.** The rollup is bumped under the same check as `times_seen`.

Buckets use the event's own `timestamp`, not `received_at`: error rate means when errors happened,
not when they reached us. Resolution is derived from the window (hourly for 24h and 7d, daily for
30d) rather than configurable, because hourly is the finest the rollup stores and a month of hourly
points is 720 values nobody can read.

One known limit: events for the same issue in the same hour contend on a single rollup row.
Irrelevant at self-hosted volume; the fix at scale is batching.

## Environments and tags

Every event's tags are rolled up per issue at ingest (`issue_tags`), with the promoted fields —
`environment`, `release`, `server_name`, `transaction` — synthesized in as tags. That one mechanism
buys three things:

- **Issue filtering**: `?environment=production` on the issue list (the dashboard grows a dropdown
  once a project reports from more than one environment), and `?tag=key:value` for everything else —
  `server_name:web-3`, `browser:firefox`, whatever the SDK sent.
- **A distribution on the issue itself**: `GET /api/v1/issues/{id}` returns tag value counts across
  *all* the issue's events, not just the latest one. "Every one of the 400 events has
  `server_name=web-3`" is a diagnosis in itself, and it is exactly what an agent should see before
  reading a single stack frame.
- **Answers that survive retention**: like `event_counts`, the rollup is written during ingest, so
  the distribution and the filters keep working after the events themselves are dropped.

Tag keys and values are capped at 200 characters and 50 tags per event, with the synthesized tags
first — an SDK sending hundreds of junk tags cannot push `environment` out.

**Users affected** rides the same rollup: the event's user identity (`id` > `username` > `email` >
`ip_address`, prefixed like Sentry's `sentry:user`) is synthesized as a `user` tag, so every issue
row and detail carries `users_affected` — a distinct-user count, not an event count. 10,000 events
on one user and 500 events on 400 users are different problems, and this is the field that tells
them apart. The tag distribution names the most-hit users, and `?tag=user:id:42` finds everything
that hit one user. Issues from SDKs that send no user context report 0.

## Retention

Two rules, whichever bites first:

| | Default | Disable with |
|---|---|---|
| `THERMITE_RETENTION_DAYS` | 90 | `0` |
| `THERMITE_MAX_EVENTS_PER_PROJECT` | 100 000 | `0` |

**Age alone does not bound disk** — a traffic spike fills it inside the window. The per-project cap is
the rule that actually protects you; age is there so "we keep 90 days" is a true statement.

**History outlives the events.** The `event_counts` and `issue_tags` rollups, the issue row
(`times_seen`, `first_seen`, `last_seen`) and any agent analyses all survive eviction, so *"this bug
happened 40,000 times over three months, all of it in production"* and its charts remain true after
the payloads are gone. That is the whole
reason the rollup is written during ingest rather than counted from `events` on read. An issue whose
events have all been evicted returns `latest_event: null` rather than failing.

Two details that matter more than they look:

- **Age is measured from `received_at`, never the event's own `timestamp`.** `timestamp` is
  client-supplied — an SDK sending 1971 would have its events evicted on arrival, and one sending
  2099 would keep them forever. There's a test for exactly that.
- **Deletes are batched** (5 000 rows, configurable). A single `delete` over millions of rows holds
  locks and bloats the table long enough to stall ingest, which is the one thing retention must not
  do.

Acked triage notifications are dropped after 30 days. **Unacked ones are never touched**, however
old: an unacked notification is work nobody did, and discarding it would hide that.

The sweep runs hourly in-process. A failed sweep is logged and retried on the next tick — falling
behind on retention is a capacity problem, not a reason to take the process down.

## The triage loop

Thermite never calls a model. It queues issues that need looking at, and whatever agent you point
at it does the thinking:

```
error → ingest → NEW issue ──→ notifications row (same transaction)
                                        ↓
                        agent: POST /triage/claim
                        agent: GET  /issues/{id}      ← full context, one call
                        agent: reads your repo, diagnoses
                        agent: POST /issues/{id}/analyses
                        agent: POST /triage/{id}/ack
                                        ↓
                        diagnosis is waiting on the issue
```

**The notification is written in the same transaction as the issue.** That's the outbox pattern —
there is no window in which an issue exists but nothing knows to look at it, and nothing is lost to
a crash in between.

**One row per issue, never per event.** A deploy that breaks something produces one unit of work,
not one per occurrence. SDK retries queue nothing. Two kinds are emitted: `new_issue` the first time
a fingerprint appears, and `regression` when a resolved issue starts happening again (which also
reopens it). Ignoring an issue stops it coming back — that's what ignoring means.

### Claims carry a lease

`claim` is atomic (`for update skip locked`), so several agents can drain concurrently and each gets
a disjoint set. The claim holds for `lease_seconds` (default 900) rather than forever, which matters
for the obvious case: a loop that fires every 10 minutes while a triage run takes 12 would otherwise
hand the same issue to the next tick. An agent that dies mid-diagnosis releases its work when the
lease expires instead of stranding it.

### Set `release` to your git SHA

This is the difference between diagnosis and guessing. Thermite hands the agent `filename`,
`lineno`, `function` and `context_line` — but reading `db.py:88` against `main` when the error came
from a three-week-old deploy produces confident nonsense.

```rust
options.release = Some(env!("GIT_SHA").into());
```

`release` is carried on every triage item, so the agent can check out the revision that actually
crashed.

Releases also change what "resolved" means. A plain resolve reopens on any recurrence — but the
usual flow is *fix committed, not yet deployed*, where the broken release keeps reporting the error
it already had. `{"status": "resolved", "in_next_release": true}` handles that: events from any
release already seen keep the issue resolved (still counted, just not news), and only a release
first reported *after* the resolve — or an event with no release, which cannot be proven old —
reopens it as a regression. Release ordering is by first sighting, the same as Sentry's: version
strings do not sort (git SHAs have no order, `1.10` < `1.9` lexically). Resolving this way without
any known release is refused rather than silently degraded to a plain resolve.

### Driving it from Claude Code

The lowest-friction setup is a loop in your application repo — no webhook, no inbound network
exposure, nothing to deploy:

```
/loop 10m Claim pending Thermite triage items. For each: fetch the issue, find the root
          cause in this working tree, post your findings back with an analysis, then ack it.
```

A push path — a signed webhook that fires `repository_dispatch` so CI can check out the exact
`release` and open a PR — is a natural addition, but the pull loop needs nothing built.

## Alerting

An error tracker nobody hears from is a dashboard you have to remember to check. Two channels,
either or both:

```sh
THERMITE_ALERT_EMAIL=you@example.com,oncall@example.com   # through the same SMTP as everything else
THERMITE_ALERT_WEBHOOK=https://example.com/hooks/thermite # one JSON POST per alert
```

Alerts ride the same outbox the triage queue reads: one row per **new issue** or **regression**,
written in the ingest transaction, so an error storm is one email rather than a thousand and a crash
cannot lose the notification. The delivery loop polls every minute and marks a row delivered only
after every configured channel succeeded — at-least-once, so an unreachable receiver means a retry,
never a lost alert. An agent acking its triage work does not silence the human alert; the two
consumers are independent.

The webhook body carries `kind`, `project`, `title`, `culprit`, `level`, `times_seen`,
`environment`, `release` and a direct `url` to the issue — enough for a Slack/Discord/ntfy adapter
without a second request. Enabling alerting on an instance with months of backlog does not flood
you: only rows from the last 24 hours are ever offered for delivery.

## MCP

`POST /mcp` is an [rmcp](https://github.com/modelcontextprotocol/rust-sdk) 3.0 Streamable-HTTP
server. It negotiates every protocol version from `2024-11-05` through `2026-07-28`, so new and old
clients both work. On `2026-07-28` the core is stateless — no session to keep alive.

Point Claude Code at it with a `.mcp.json` in your application repo:

```json
{
  "mcpServers": {
    "thermite": {
      "type": "http",
      "url": "https://thermite.example.com/mcp",
      "headers": { "Authorization": "Bearer ${THERMITE_TOKEN}" }
    }
  }
}
```

Ten tools, each a thin presenter over the same functions the REST handlers call — the two surfaces
cannot drift apart:

| | |
|---|---|
| `list_projects` `list_issues` `get_issue` `get_event` | Read |
| `project_stats` | Rate over time and current state |
| `pending_triage` `claim_triage` `ack_triage` | The queue |
| `post_analysis` `set_issue_status` | Write back |

Mistakes an agent makes — a missing issue, a bad window — come back as **tool-level error results**
carrying the reason, not as protocol errors. The agent reads the message and corrects itself instead
of seeing what looks like a broken transport. Only auth failures and infrastructure faults are
protocol errors.

## Grouping

Ported from Bugsink's `bugsink-v2` mechanism:

1. An explicit `fingerprint` wins. `{{ default }}` inside one is substituted with the computed key
   rather than taken literally, so SDKs can refine default grouping instead of replacing it.
2. Otherwise the key is the exception type plus its **normalized** value — or the log message, for
   events with no exception. The last exception in a chain is the one used.

Normalization (Sentry's `parameterization.py`, ported) replaces variable parts of the message:
`could not reach 10.0.0.7 after 30s` → `could not reach <ip> after <duration>`. Without it every
error carrying an id, an address or a count opens its own issue and the list becomes unusable.

Note that v2 deliberately dropped `transaction` from the key. v1 grouped on `title ⋄ transaction`,
which split one bug across every route that could trigger it.

Two deviations from upstream, both marked in `src/protocol/normalize.rs` — upstream's `duration`
pattern matches only a single digit before `s`, so `30s` and `45s` normalize *differently*, and its
`ip` alternatives are ordered such that a compressed IPv6 address is only partially consumed. Both
defeat the purpose of the pass, so both are fixed here.

## Compatibility boundary

**Stored:** error and message events, exception chains, stack frames with source context,
breadcrumbs, contexts, tags, user, request, sdk, release, environment, transaction, server_name,
explicit fingerprints, and any other field the SDK sends.

**Accepted and dropped** — a `200`, but not stored: sessions, transactions, logs, metrics, client
reports, attachments, profiles. Rejecting an envelope because it contains a session item would break
every real SDK, so these are counted, logged at debug, and discarded.

**Not implemented yet:** attachments and minidumps, CSP/security reports, release health
(sessions/adoption), orgs/teams, and any server-side LLM analysis.

Duplicate `event_id`s are deduplicated per project, so an SDK retry does not inflate `times_seen`.
Out-of-order delivery does not let an older event overwrite an issue's display fields, and an event
timestamp more than an hour ahead of the server clock is clamped — otherwise one client with a bad
clock pins its issue to the top of the list forever.

## Development

```bash
docker compose up -d postgres
cargo test      # unit tests need no database; #[sqlx::test] spins up one per test
cargo clippy --all-targets
```

`tests/sdk_compat.rs` is the test that matters most: it binds a real socket, points an unmodified
`sentry` crate client at it, and asserts what lands in the database. Everything else only proves
Thermite is self-consistent.

### A note on sqlx

The application crate uses the compile-time-checked `query!` macros with committed `.sqlx` metadata,
so a fresh clone builds with no database running. After changing any SQL there, run
`cargo sqlx prepare -- --no-default-features --features server`.

`thermite-core` deliberately does **not**: its SQL is assembled from shared column-list constants
(`ISSUE_COLUMNS`, `EVENT_COLUMNS`), and the macros only accept literal strings. Inlining those
constants at every call site to satisfy the macro would trade one source of truth for a dozen copies.
Its queries are covered by the integration tests instead.

## Known limitations

`fingerprint_hash` lives directly on `issues` rather than in a separate join table. Bugsink uses a
join table so it can migrate grouping algorithms and merge issues without re-splitting history.
There is only one algorithm here so far — but changing the grouping rule later **will** create new
issues alongside the old ones. Cheap to add when a second algorithm actually exists.

## Licence

Thermite is [AGPL-3.0](LICENSE). Run it, read it, modify it, self-host it for anything — including
commercially, inside your own company — at no cost. What the licence asks in return is that if you
modify Thermite and offer *that* to others over a network, you publish your changes.

`thermite-sdk` is **MIT OR Apache-2.0**, deliberately not AGPL. It is linked into your application,
where copyleft would reach through into your own code. Every Sentry SDK is permissive for the same
reason.

If AGPL does not work for you — a fork you cannot publish, or a legal policy that rules it out — a
commercial licence is available: [mail@haukejung.de](mailto:mail@haukejung.de).

Contributions are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md), and the [CLA](CLA.md) that keeps
that second licence possible to offer. You keep your copyright; it is a licence grant, not an
assignment.
