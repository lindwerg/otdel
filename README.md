# OTDEL

> Status: **design/prototype planning stage** (2026-09-13). This is not a
> running application yet. There is no Rust workspace, no web app scaffold,
> and no Docker services in this repository yet. Nothing below should be read
> as "it's built" — it describes what exists and what is planned, honestly.

OTDEL is planned as a system that turns a partner's raw materials (catalogs,
presentations, spec sheets) into a verified, versioned product knowledge base:
ingestion → structure extraction → drafting → limited industry research →
verification → automatic publication of a knowledge version, with sources and
gaps tracked throughout. The current specification for the first block
("partner and product knowledge base") is in
[docs/block-01-spec.md](docs/block-01-spec.md); background and the initial
scoping decisions are in [docs/project-start.md](docs/project-start.md).

Planned target: public repository at
[github.com/lindwerg/otdel](https://github.com/lindwerg/otdel).

## Planned stack (not yet scaffolded)

- **Backend**: Rust (proposed: Tokio, Axum, SQLx), running as a server and
  background worker process from one codebase.
- **Database**: PostgreSQL, with the pgvector extension (in the same
  database, not a separate store) for semantic search.
- **Web**: eventually TypeScript/React; not started.
- **Files**: an S3-compatible store for original documents and derived pages
  (planned; not configured today).

These are engineering choices from the spec, not commitments about specific
crate/library versions — see [docs/block-01-spec.md](docs/block-01-spec.md)
§3 and §14 for what is still an open decision and why no Docker services or
provider credentials exist yet.

## What actually exists in this repository today

- `docs/` — product/process specs and planning notes (`block-01-spec.md`,
  `project-start.md`, `development.md`, plus this-session's worker/config
  notes). `docs/block-01-plan.md` and `docs/block-01-design.md` are owned by
  other parallel work; this worktree/branch may simply not be synced yet, so
  their absence here is not permanent. Links to
  them will only work once they land.
- `design/` — a static, standalone HTML/CSS/JS prototype of the interface
  (developed as a separate, parallel task) plus the Otto mascot assets; see
  [design/ASSET-PROVENANCE.md](design/ASSET-PROVENANCE.md). It is a visual
  prototype only — it is not wired to any backend.
- Repository/process tooling owned by this task: `Makefile`, `scripts/`,
  `.github/`, `.editorconfig`, `.env.example`, `CONTRIBUTING.md`, this
  `README.md`.

## Quick start

```sh
make check    # repository hygiene checks — Node.js >= 22, no install needed
make preview  # serve design/ only, at http://127.0.0.1:4173
```

`make check` runs `scripts/check.mjs`: tracked-file hygiene (no committed
`.env`/private data/PDFs/ZIPs/keys, no obvious credential patterns, no
unresolved merge conflicts), JSON/JS syntax validation, `git diff --check`,
and minimal presence/shape checks for the core docs. It is a lightweight
hygiene check for this stage, not a linter or test runner, and it does not
claim to be an exhaustive secret scanner. Full details:
[docs/development.md](docs/development.md).

`make preview` serves **only** the `design/` directory (nothing else in the
repository) on `127.0.0.1:4173` using Python's standard `http.server`, so
nothing is exposed beyond that one directory or beyond the local machine.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for the mechanics and
[docs/development.md](docs/development.md) for the full workflow: branching
and worktrees, review/CI expectations for a protected `main`, and how
secrets/private data (BASIS catalogs, company datasets) are kept out of the
repository.

## Specs and planning docs

- [docs/block-01-spec.md](docs/block-01-spec.md) — current specification for
  block 1 (partner & product knowledge base).
- [docs/project-start.md](docs/project-start.md) — initial scoping decisions
  and material review notes.
- [docs/development.md](docs/development.md) — day-to-day development
  workflow for this repository.
- `docs/block-01-plan.md` and `docs/block-01-design.md` — owned by other
  parallel work; may not be present in this worktree/branch until synced.
