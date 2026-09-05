# Contributing to Thermite

Thanks for taking the time. Issues, bug reports and discussion are always welcome. Pull requests are
too — with one condition, in [Licensing](#licensing) below, which exists so Thermite can stay open
source without giving up the hosted version that pays for it.

## Before you write code

**Open an issue first for anything non-trivial.** A typo fix or an obvious bug needs no ceremony. A
new feature, a schema change, or a change to how ingest, grouping or retention behaves does — those
carry invariants that are not obvious from the code, and it is much cheaper to talk about the
approach than to review a finished branch that has to be rewritten.

`CLAUDE.md` is the architecture document, and it is unusually specific about *why* things are the way
they are: why ingest is synchronous, why authentication strictly precedes decoding, why the dashboard
reads rollups rather than counting rows, why the pools are split, why grouping deviates from upstream
in two named places. Read the section covering the area you are touching. Several of those decisions
look like something to optimise and are not.

## Getting set up

```sh
docker compose up -d          # PostgreSQL + Mailpit          (just init)
just bootstrap                # .env with a fresh SESSION_SECRET
dx serve                      # dashboard, docs, ingest, /api/v1, /mcp   (just serve)
```

You need a Rust toolchain (the version in `rust-toolchain.toml`), the
[Dioxus CLI](https://dioxuslabs.com/learn/0.7/getting_started), [Bun](https://bun.sh) and
[just](https://github.com/casey/just). PostgreSQL comes from `compose.yaml` on host port 5433, so it
will not collide with any other Postgres you have running.

## Before you open the pull request

```sh
just check    # cargo fmt --check, clippy -D warnings, and both test suites
```

That mirrors what CI runs. CI additionally runs `cargo machete` for unused dependencies, `cargo
audit` for vulnerable ones, a wasm build of the client, and a job that builds every query against a
real schema. If `just check` is green locally, the remaining jobs rarely surprise you.

Two things that catch people out:

- **The app crate needs its features named explicitly.** `cargo test -p thermite` builds the WASM
  client by default, which has no tests and cannot reach a database. Use
  `--no-default-features --features server`. `just test` does this for you.
- **Tailwind has to be built before clippy** when `dx serve` is not running, or the stylesheet
  asset is missing:
  ```sh
  bun install --frozen-lockfile
  bunx @tailwindcss/cli -i tailwind.css -o assets/tailwind.css   # just tw
  ```

### If you touched SQL

Run `just prepare` against a live database to regenerate the committed `.sqlx` metadata, and commit
the result. Without it the next offline build fails.

**Migrations are immutable once applied.** sqlx records a checksum of each file, so editing an
applied migration — even a comment in it — fails the next boot. Add a new numbered migration instead.

### If you touched the UI

Look at it. `dx serve` and open the page. Compile-clean is not the same as correct for anything with
a layout.

## Style

- **Match the surrounding code.** Thermite is fairly opinionated and fairly consistent; a patch that
  reads like the file it lands in is easier to review than one that is individually nicer.
- **Comments explain why, not what.** The codebase leans on this heavily — most non-obvious lines
  carry the reason they are that way. If your change makes an existing comment wrong, fix the
  comment.
- **Commit messages use [Conventional Commits](https://www.conventionalcommits.org/):**
  `<type>(<scope>): <description>` — imperative, lowercase, no trailing period. `feat(ingest): cap
  tag cardinality per issue`.
- **Keep the diff to the change.** Unrelated refactors, reformatting and drive-by "improvements" make
  a patch hard to review and hard to revert. If you spot something else worth fixing, say so in the
  issue.

## Tests

New behaviour needs a test, and a bug fix needs a test that fails before the fix.

- `crates/thermite-core/tests/` — protocol, grouping, ingest, stats and triage, driving the routers
  directly. These need `#[sqlx::test(migrations = "../../migrations")]`, since the migrations live at
  the workspace root rather than beside the crate.
- In-crate `#[cfg(test)]` modules in the app, because a binary crate cannot be imported from
  `tests/`. Large test modules live in a sibling `*_tests.rs` file wired up with
  `#[cfg(test)] #[path = "..."] mod tests;`.

The test that matters most is the one pointing an **unmodified** `sentry` client at the ingest
endpoint. Everything else only proves Thermite is self-consistent; that one proves it is still
Sentry-compatible. Do not break it to make a change easier.

## Licensing

Thermite is licensed under the AGPL-3.0:

| | Licence | |
|---|---|---|
| `thermite`, `thermite-core` | **AGPL-3.0** | The server, dashboard and ingest |

Contributions are accepted under the [Thermite CLA](CLA.md). **You keep your copyright** — it is a
licence grant, not an assignment. It exists so a commercial licence can be offered to organisations
whose legal policy rules out the AGPL, which requires one party to hold the necessary rights across
the whole codebase.

Sign it by including this in the description of your first pull request:

```
I have read and agree to the Thermite CLA (CLA.md).
Signed-off-by: Your Name <your@email.example>
```

One signature covers all your later contributions. If your employer owns the rights to your work,
email <mail@haukejung.de> first — that needs a corporate agreement instead.

## Security

**Do not open a public issue for a security vulnerability.** Email <mail@haukejung.de> with the
details and give it a reasonable window before disclosing.

## Questions

Open an issue, or email <mail@haukejung.de>.
