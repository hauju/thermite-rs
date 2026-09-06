# Security policy

## Reporting a vulnerability

**Do not open a public issue.**

Report privately through either channel:

- **GitHub** — the *Report a vulnerability* button under the Security tab.
- **Email** — <mail@haukejung.de>. Encrypt if you prefer; ask and a key will be sent.

Please include enough to reproduce it: the affected endpoint or component, the version or commit,
what an attacker gains, and a proof of concept if you have one. A `file:line` pointer is worth more
than a scanner report.

## What to expect

Thermite is maintained by one person, so here are honest numbers rather than an enterprise SLA:

| | |
|---|---|
| Acknowledgement | within 5 working days |
| Initial assessment | within 14 days |
| Fix for a confirmed critical issue | as fast as it can be done properly |

You will be told what the assessment concluded, including when a report is not accepted and why. If
you would like credit in the release notes, say so — and say how you want to be named.

**Coordinated disclosure**: please give 90 days from acknowledgement, or until a fix ships, whichever
comes first. If a report is being handled too slowly, say so before disclosing; that is fair warning
and it will be taken seriously.

## Supported versions

Thermite has no tagged releases yet. **Only the latest commit on the default branch is supported.**
Self-hosted operators should track it. Once releases exist, this section will name the supported
ones.

## Design decisions that are not vulnerabilities

These are deliberate and documented. Reports about them will be closed as by-design, though an
argument that one of them is *wrong* is welcome as a normal issue.

- **The DSN public key is not a secret.** It is embedded in client applications by design and grants
  exactly one capability: sending events to one project. It cannot read anything.
- **Ingest is reachable by anything on the internet, without a session.** That is what an error
  tracker is. It authenticates on the DSN key alone, and deliberately carries no session or CSRF
  token.
- **Thermite is single-tenant. Every account is a full operator over every project.** There is no
  per-user authorization model and no owner column on `projects`. This is why registration is closed
  by default — on a fresh instance only the first account may register, and `THERMITE_ALLOWED_EMAILS`
  / `THERMITE_ALLOWED_EMAIL_DOMAINS` govern it thereafter. Do not run a shared instance for people
  who should not see each other's errors.
- **Rate limits are approximate.** Per-IP and per-project quotas are counted in-process, so a
  multi-node deployment enforces roughly N times the configured limit. Enforcing an approximate quota
  exactly would cost a network round trip per event.
- **Volumetric denial of service from an authenticated or quota-bearing client.** Capacity is an
  operator concern.
- **Anything requiring operator access to an instance you control**, or a misconfiguration of your
  own deployment — missing TLS, an exposed database port, a weak `SESSION_SECRET`.

## Particularly interesting

If you are looking for somewhere to start, these are the areas where a real problem would matter
most:

- **Cross-project data access.** Any path where one project's DSN key, API key or OAuth token reaches
  another project's issues, events or configuration.
- **Unauthenticated work amplification in ingest.** A small request that causes large memory,
  storage or CPU cost before the DSN key is checked. Authentication strictly precedes body decoding
  and envelope parsing for exactly this reason, and the item list is capped — a way around either is
  a genuine finding.
- **Unbounded permanent storage.** One request creating unbounded rows in `issues`, `issue_tags`,
  `event_counts` or `releases`, all of which outlive event retention.
- **Auth bypass** on `/api/v1`, `/mcp`, or the dashboard — including the OAuth 2.1 flow used by
  claude.ai connectors (PKCE downgrade, redirect-URI bypass, code replay).
- **Secret exposure**: API keys or session material leaking into responses, logs, or error events.
  Event payloads are scrubbed before storage; a way past the scrubber counts.
- **Injection**: SQL reaching the database from event payloads, or stored XSS from event content
  rendered in the dashboard.

## For operators

Running your own instance:

- Set a strong `SESSION_SECRET` (`just bootstrap` generates one) and keep the registration allowlist
  configured.
- Terminate TLS in front of Thermite, and set `TRUST_PROXY_HEADERS` only when a proxy you control is
  actually rewriting `X-Forwarded-For` — trusting it otherwise lets a client forge its own rate-limit
  identity.
- Point `THERMITE_DSN` self-reporting at a *different* instance or project. An instance reporting its
  own ingest failures into the ingest that is failing loses exactly the events worth having.
- Do not expose the PostgreSQL port. `compose.yaml` binds it to `127.0.0.1` for this reason.
- `THERMITE_DEMO_PROJECT` makes one project readable by anyone — every stack trace, breadcrumb
  and user context in it. Leave it unset unless that project only ever receives synthetic or
  public data.
