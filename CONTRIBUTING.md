# Contributing

OTDEL is at the design/prototype planning stage. This file covers the
practical mechanics of contributing; see [docs/development.md](docs/development.md)
for the full workflow (branching, worktrees, PR/review/CI expectations,
secrets handling) and [docs/block-01-spec.md](docs/block-01-spec.md) for the
current product specification.

## Before you start

1. Read [AGENTS.md](AGENTS.md) and [docs/development.md](docs/development.md).
2. Create a dedicated branch and, if you are working alongside other
   in-progress tasks, a dedicated `git worktree`, from an up-to-date and
   clean `main`.
3. Confirm your baseline is clean before making changes:
   ```sh
   git status
   ```

## Making a change

- Keep changes scoped to the task/area you were assigned. Do not edit files
  owned by another concurrent task/worker.
- Run the repository check before opening a pull request:
  ```sh
  make check
  ```
- If your change touches the static prototype under `design/`, preview it
  locally:
  ```sh
  make preview   # http://127.0.0.1:4173, serves design/ only
  ```

## Opening a pull request

- Use the pull request template (filled in: task, scope, tests with real
  output, screenshots for UI changes, limitations).
- All required CI checks must pass on the PR.
- Changes land on `main` only via a reviewed pull request — not by pushing
  directly to `main`. See [docs/development.md](docs/development.md) for how
  review and branch protection are set up for this repository.

## What not to do

- Do not commit `.env` files, private materials, or company datasets
  (`private/`, `data/`, `materials/`, `*.pdf`, `*.zip`, `*.pem`, `*.key` are
  git-ignored on purpose — see [.gitignore](.gitignore)).
- Do not force-push or reset shared branches.
- Do not add application scaffolding (Rust workspace, web app, Docker
  services) outside of an explicitly scoped, reviewed task — see
  [docs/development.md](docs/development.md#no-application-scaffold-or-docker-services-yet).

## Reporting issues / proposing tasks

Use the issue templates under `.github/ISSUE_TEMPLATE/`: "Bug report" for
something broken, "Implementation task" for proposing or tracking a concrete
piece of work.
