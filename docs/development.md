# Development workflow

Status: this repository is at the design/prototype planning stage
(2026-09-13). There is no Rust workspace, no Cargo.toml, no package.json, and
no Docker services yet. This document describes how contributors (human or
Claude Code workers) work in this repository today, and what changes once the
application scaffold (phase 1A of [block-01-spec.md](block-01-spec.md)) lands.

## Quick start

```sh
make check    # repository hygiene checks (Node >= 22, no install needed)
make preview  # serve design/ only, at http://127.0.0.1:4173
```

See [../README.md](../README.md) for a short overview and links to the
product/design specs.

## Branching and worktrees

- Every task gets its own branch and its own `git worktree`, created from a
  verified, clean `main`. Do not do parallel unrelated work in a single
  checkout.
- Branch naming used so far in this project: `claude/<area>` (e.g.
  `claude/design-system`, `claude/dev-platform`, `claude/qa-review`). Pick a
  name that matches the owned-files area of the task.
- Before starting a task, verify the baseline is clean:
  ```sh
  git status            # expect: nothing to commit, working tree clean
  git log -1 --oneline  # confirm you are on the expected base commit
  ```
- Workers (Claude Code) do not push, do not open pull requests, and do not
  merge or integrate branches themselves. A worker's job ends at handing back
  a reviewed-ready local diff/commit in its own worktree and reporting on it.
  ASTRA (the Codex-led development system that owns integration — see
  [AGENTS.md](../AGENTS.md)) performs the push, opens the PR, and carries out
  integration into `main` **after** independent review, not the worker.
  Workers never run `git push --force` or `git reset --hard` against shared
  history either way.
- A worker stays within its owned-files scope for the task it was given. It
  does not revert or rewrite files that belong to another concurrent worker's
  area, even if those files look incomplete — a missing file another worker is
  expected to add later is not this worker's job to fill in speculatively.

## Reporting

- A worker reports results (files changed, exact verification commands and
  their real output, known risks/limitations) at the end of the **same**
  session that did the work, in the format requested by the task. It does not
  start a second, parallel session for the same area to "double check".
- If a reviewer asks for a small, routine fix on already-reviewed work (e.g.
  a typo, a lint nit), that fix does not require the human project owner to
  re-approve the whole task from scratch — routine follow-up fixes are
  reviewed like the rest of the diff, not escalated by default.

## Integration: reviewed PRs, combined checks, protected `main`

- Changes reach `main` only through a pull request that has been reviewed
  (by the project owner or an independent reviewer role), not through direct
  pushes to `main`. Workers do not push their own branch or open the PR (see
  above); ASTRA pushes the reviewed branch and opens/integrates the PR after
  review, on the worker's behalf.
- `main` is intended to be a protected branch. CI (`repository-check`, and
  later the conditional Rust/web jobs described below) must pass on the PR
  before merge; all required checks are combined into one status a reviewer
  looks at, not inspected job-by-job.
- Required-approval count is expected to be configured as **0 external
  approvals** in branch protection for this repository, because GitHub does
  not allow an account to approve its own pull request, and today the only
  account involved is the project owner's own account. This is a documented
  fact about how GitHub review works, not a claim that review is skipped —
  human review still happens, it is just not enforced via GitHub's "required
  approving reviews" counter while there is a single account. If a second
  reviewing account is added later, this setting should be revisited.
- After a worker's branch is merged, sync `main` locally (`git fetch` +
  fast-forward or recreate the worktree from the updated `main`) **before**
  starting the next task from that worktree. Do not keep building on a stale
  base.

## What `make check` actually does (and does not do)

`make check` runs `scripts/check.mjs` (Node.js >= 22 built-ins only, no
dependency install required). It only looks at files tracked by git
(`git ls-files`) — **new files must be `git add`-ed first**, or the check
won't see them locally. A pull request's CI run always checks out the branch
with everything already committed, so this is enforced automatically there;
locally, run `git add` (or `git status` to confirm what's tracked) before
`make check` if you just created files.

It performs:

1. Node.js version check.
2. Tracked-file hygiene: rejects committed `.env` files (other than
   `.env.example`), anything under `private/`, `data/`, `materials/`, or
   `.local/` (rejected even though `.local/` is git-ignored, in case it is
   ever force-added), and `*.pdf` / `*.zip` / `*.pem` / `*.key` files, and
   committed `docs/worker-runs/` logs.
3. Tracked symlinks are rejected outright, anywhere in the repo — so
   `make preview`'s plain static file server can never be pointed at a link
   that escapes `design/`.
4. A **best-effort, non-exhaustive** scan for a few obvious credential shapes
   (AWS access key IDs, PEM private key headers, Slack/GitHub tokens) in
   tracked text files. This is a safety net, not a substitute for not
   committing secrets in the first place. Known binary asset extensions are
   skipped; anything else that can't be read as text, or that is larger than
   the scan size limit, **fails the check** instead of being silently
   skipped.
5. Unresolved merge-conflict markers in tracked text files.
6. JSON syntax validation for every tracked `*.json` file.
7. JavaScript syntax validation (`node --check`) for every tracked
   `*.js`/`*.mjs`/`*.cjs` file.
8. `git diff --check` for whitespace issues (best-effort; only meaningful
   when there is a local diff).
9. Presence and minimal shape of the docs this worker owns
   (`README.md`, `CONTRIBUTING.md`, `.editorconfig`, `.env.example`,
   `docs/development.md`, plus the pre-existing `AGENTS.md` and
   `docs/block-01-spec.md`). Docs owned by other parallel work
   (`docs/block-01-design.md`, `docs/block-01-plan.md`) are validated only if
   present in the current checkout — a worktree/branch that hasn't synced
   with `main` yet simply won't have them, which is not itself a failure.
10. Manifest-vs-CI enforcement gate: **fails** if `Cargo.toml`,
    `package.json`, or `apps/web/package.json` exists but
    `.github/workflows/ci.yml` doesn't yet contain the matching real
    build/lint/test commands — see the CI section below.

`make check` does **not** run Rust or web application tests, because no such
code/manifests exist yet. See the CI section below for how that changes.

## What `make preview` actually does

`make preview` runs `python3 -m http.server 4173 --bind 127.0.0.1 --directory
design`. This serves **only** the contents of `design/` (the static
prototype: `index.html`, `prototype.js`/`prototype.css`, `assets/`), bound to
`127.0.0.1` (localhost only, never `0.0.0.0`), so nothing outside `design/`
and nothing beyond the local machine is exposed by this command.

## CI

`.github/workflows/ci.yml` defines one stable job today, `repository-check`:
checkout (pinned to a full commit SHA, not a mutable tag) → setup Node.js 22
(also pinned to a full commit SHA) → `make check`. Action versions are pinned
by commit SHA specifically so a compromised or retagged action release cannot
silently run different code than what was reviewed; if a SHA cannot be
independently verified against the action's published release, only a
previously-established, known-official SHA is used, and the workflow does not
otherwise fetch or execute unverified third-party code.

The workflow also explicitly reports (as CI notices, not silently) that Rust
and web test suites are **pending**, not run: there is no `Cargo.toml` and no
`package.json` in the repository yet. This is intentional — a CI step that
pretends to test code that doesn't exist would be misleading.

### Adding real Rust/web jobs later

Once the corresponding manifest exists, the PR that introduces it must also
update `.github/workflows/ci.yml` to add a real job, for example:

- **Rust** (once `Cargo.toml` exists): checkout → pinned Rust toolchain
  action → `cargo fmt --check` → `cargo clippy --all-targets -- -D warnings`
  → `cargo test`. Gate the job on `Cargo.toml` being present so it doesn't
  fail on branches that don't touch the Rust workspace yet.
- **Web** (once `package.json` exists): checkout → pinned `setup-node` →
  `npm ci`/`pnpm install --frozen-lockfile` → lint → build → test. Gate the
  job on `package.json` being present.

Both should be added as genuinely-run steps, not placeholders that report
success without doing anything.

## Environments, secrets, and private data

- `.env` is git-ignored; `.env.example` contains placeholder values only and
  is committed. Copy `.env.example` to `.env` locally once something actually
  reads it — nothing does yet.
- No provider credentials (AI model provider, external search provider, S3,
  etc.) are required for today's design/prototype work. Introducing a real
  provider is a phase-1A-or-later decision (see
  [block-01-spec.md](block-01-spec.md) §3, §14), not something the tooling in
  this PR needs.
- The BASIS product catalogs and the raw company datasets (RSMP/revenue-
  expense/debt/tax-regime/headcount registries mentioned in
  [project-start.md](project-start.md)) are private source material. They are
  never committed to this repository and never published, regardless of
  branch or worktree; `.gitignore` already excludes `data/`, `materials/`,
  and `*.pdf`/`*.zip`.

## No application scaffold or Docker services yet

This repository intentionally does not yet contain a Rust workspace, a web
app scaffold, or `docker-compose` services (PostgreSQL/pgvector, S3-compatible
storage, etc.). Per [block-01-spec.md](block-01-spec.md) §3 and §14, those
choices (concrete crates, storage provider, OCR/model adapters) are made when
phase 1A ("Приём") is actually implemented, not before. Adding them now would
create infrastructure with no measured need and no reviewed design behind it.
`.env.example` documents the variable *names* expected at that point purely
so contributors aren't surprised later — it is not a signal that phase 1A has
started.
