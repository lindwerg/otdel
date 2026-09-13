# Backend phase 1A — running it, and what it actually does

Scope: the intake half of block 1 — sessions for the single local owner, partner cards,
streaming upload of original PDF/PNG/JPEG files, authorised download of those originals,
and the recorded extraction queue. The API is exactly
[`implementation-contract.md`](implementation-contract.md); nothing beyond it is
implemented.

**Reading documents is phase 1B**, and it now exists — see
[`extraction-1b.md`](extraction-1b.md). Within *this* phase an uploaded file is stored,
hashed and queued and nothing more: its status stays `queued`, `page_count` is `null`, and
no text is produced until the worker picks it up.

## Quick start

The toolchain is pinned: Rust **1.98.1**, in `rust-toolchain.toml` and in
`.github/workflows/ci.yml` (`RUST_VERSION`) — bump the two together, never one alone.
`make` targets prepend `~/.cargo/bin` to `PATH` and resolve `cargo` to a full path, so
they work on a stock macOS shell without editing a profile.

```bash
make dev-init     # random local credentials into .local/ (the password is not printed)
make db-up        # PostgreSQL 17 + pgvector on 127.0.0.1:58432, compose project otdel-block1
make migrate      # schema, applied with the migration role
make bootstrap    # provision the `local` bureau
make server       # API on http://127.0.0.1:18480
make worker       # maintenance worker (optional, separate terminal)
```

The owner password is written to `.local/owner-password.txt` (mode 0600) and deliberately
not echoed to the terminal:

```bash
cat .local/owner-password.txt
```

`make dev-init` refuses to overwrite existing credentials; `./scripts/dev-init.sh --force`
regenerates them, which invalidates the database roles already created.

## Layout

| Path | What lives there |
|---|---|
| `crates/otdel-core` | Domain model, configuration, validation, secrets. No I/O, fully unit-testable. |
| `crates/otdel-storage` | `ObjectStore` trait + local filesystem backend (streaming, hashing, staging). |
| `crates/otdel-db` | PostgreSQL: bureau-scoped transactions, partners, materials, jobs, sessions, pages. |
| `crates/otdel-extract` | Phase 1B: PDF text layer (pure Rust), layout/tables, OCR adapters. |
| `crates/otdel-llm` | Phase 1C: the single model adapter — bounded calls, redacted logs, an honest "not configured". |
| `crates/otdel-knowledge` | Phase 1C as a pure pipeline: source catalogue, prompt, quotation matching, validation. |
| `crates/otdel-search` | Phase 1D: the outward-facing adapters — configured search endpoint, SSRF-hardened fetcher. |
| `crates/otdel-research` | Phase 1D as a pure pipeline: query building, external catalogue, budget arithmetic, validation. |
| `crates/otdel-api` | Axum router, session/CSRF, upload/download handlers, integration tests. |
| `crates/otdel-worker` | Extraction (1B), understanding (1C) and research (1D) + recovery maintenance. |
| `apps/api`, `apps/worker` | Thin binaries: `otdel-api`, `otdel-worker`. |
| `migrations/` | SQL, applied by the migration role only. |
| `infra/postgres/initdb/` | Role and test-database creation on first container start. |

## Commands

```
otdel-api serve          # default; HTTP API
otdel-api migrate        # migrations, needs OTDEL_DATABASE_ADMIN_URL
otdel-api bootstrap      # provision the configured bureau
otdel-api hash-password  # reads a password on stdin, prints the Argon2 hash
otdel-api check-config   # loads and validates the environment, prints it redacted

otdel-worker run         # extraction + maintenance until stopped
otdel-worker once        # one pass of each, then exit
otdel-worker probe       # report OCR/rasteriser availability, then exit
```

## Security decisions worth knowing

**Two database roles.** `otdel_migrator` owns the schema and applies migrations;
`otdel_app` runs the API with no ownership, no `BYPASSRLS` and no `CREATE*` rights. The
tenant tables use `ENABLE` **and** `FORCE ROW LEVEL SECURITY`, and every request opens a
transaction that sets `otdel.bureau_id` with `set_config(..., is_local => true)`, so a
pooled connection cannot leak context into the next request. Without a context, policies
match nothing (fail closed).

At startup the API asks the database what it actually is — superuser, `BYPASSRLS`, table
owner, whether `row_security_active` is true on each tenant table, whether it can read
`otdel.sessions` — and refuses to serve if any answer would make isolation cosmetic.
Naming a role in a migration is not evidence; this check is.

**Sessions.** Opaque 32-byte tokens in an `HttpOnly; SameSite=Strict` cookie; only their
SHA-256 fingerprint is stored. The CSRF token is a separate random value returned in the
JSON body (never in a cookie) and required on every state-changing request, alongside an
`Origin` check. The runtime role has *no* privileges on the sessions table at all — it
can only call `SECURITY DEFINER` functions, so authentication works before any bureau
context exists and a bug in the application cannot forge session rows.

**Login throttling.** Attempts are counted *before* the Argon2 verification (so a
concurrent burst is bounded, not just a sequence), and the number of simultaneous hashes
is capped by a semaphore whose permit is moved into the blocking task — a client that
disconnects mid-hash does not free capacity while the CPU work continues.

**Originals.** Storage keys are `bureau-<uuid>/partner-<uuid>/<shard>/<sha256>`: derived
from identifiers the server controls plus the content hash, never from the uploaded file
name (which is kept for display only, with any path stripped). The tree is never served
as a static directory; the download route resolves a database-stored key, re-validates
it, and refuses anything that is not a regular file inside the storage root. Uploads are
streamed, size-limited while streaming, and accepted only if the *content signature* is
PDF/PNG/JPEG — a contradicting declared `Content-Type` is rejected rather than trusted.

**Deduplication and durability.** `(partner_id, sha256)` is unique per partner: the same
bytes again return `200` with the existing material and never enqueue a second job; a
changed file is a new material (originals are never overwritten). The object is written
and fsynced first, then the material row and its queue entry are committed together.

If that commit fails, the staging file of *this* request is removed, and the finalized
object is **retained and reported**, not deleted. Deleting it would be unsafe: the key is
content-addressed, so a concurrent upload of the same file may already have adopted the
same object, and a “check the database, then delete” sequence loses that race. The
maintenance worker reports such orphans for the same reason instead of collecting them.

## Maintenance worker

The recovery half of `otdel-worker`: expired/idle sessions are purged, jobs whose lease
expired go back to `queued` (or `failed` at the attempt limit), staging files from
interrupted uploads are swept, and orphan objects are counted and logged. The extraction
half that consumes the queue is documented in [`extraction-1b.md`](extraction-1b.md).

## Tests

```bash
make check     # hygiene + fmt + clippy + tests that need no database
make test-db   # provisions otdel_test and runs the whole suite
```

Unit tests (core, storage, api, worker) need nothing. The API integration suites need a
real PostgreSQL, because row-level security, the composite foreign keys and the
`SECURITY DEFINER` session functions are precisely what they check. They read:

| Variable | Role |
|---|---|
| `OTDEL_TEST_DATABASE_URL` | restricted runtime role on `otdel_test` |
| `OTDEL_TEST_ADMIN_DATABASE_URL` | migration role on `otdel_test` |
| `OTDEL_TEST_SUPERUSER_URL` | superuser; only the “privileged role is refused” test |

`scripts/dev-test-db.sh` provisions and prints them:

```bash
exported="$(./scripts/dev-test-db.sh --export)" && eval "$exported" && cargo test --workspace
```

Note the assignment before `eval`: `eval "$(...)"` would hide a failure of the script.

**The test database is separate on purpose.** Everything the suite does happens in
`otdel_test`; the pilot database `otdel` is never migrated, truncated or written to by
tests. When the variables are missing the tests **fail with that explanation** rather
than passing quietly — a database test that skips itself is worse than no test.

Test fixtures write through the same row-level-security context the server uses
(`TestApp::admin_tx`), because `FORCE ROW LEVEL SECURITY` applies to the schema owner
too. No test weakens a policy to make itself pass.

No documents are committed: the suites generate tiny synthetic PDF/PNG/JPEG bytes in
process.

## Migrations

Applied with the migration role only, by `otdel-api migrate`. Re-running is a no-op.

That last property depends on one subtlety: SQLx records applied versions in an
*unqualified* `_sqlx_migrations` table, so `search_path` decides which schema it lands
in. With the role's default (`otdel, public`) the first run wrote the history to `public`
(the `otdel` schema did not exist yet) and a second run looked in `otdel`, found nothing
and replayed everything — failing with “function current_bureau_id already exists”. The
migration connection now pins `search_path=public`, making `public._sqlx_migrations` the
single canonical history; every statement in `migrations/` is schema-qualified, so
nothing else depends on it. There is a regression test for repeated application.

Migrations `0001`/`0002` have been applied to the pilot database with real data. They are
frozen: further schema changes go into new migration files, and recorded checksums are
never rewritten. Phase 1B adds `0003_extraction.sql` (pages, source regions, table cells,
plus the extraction bookkeeping columns) as a separate, additive file for exactly that
reason.

## pgvector

The container image ships pgvector, and `/ready` reports its real state
(`installed` / `available_not_installed` / `absent`). Phase 1A stores no embeddings, so
the migrations deliberately do **not** create the extension — the readiness output states
the situation instead of implying that semantic search exists.

## Ports and isolation

API `127.0.0.1:18480`, PostgreSQL `127.0.0.1:58432`, compose project `otdel-block1`,
volume `otdel-block1-pgdata`. Nothing binds to `0.0.0.0`, and no container of another
project is touched. Originals live under `.local/storage`, which is git-ignored.
