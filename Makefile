.PHONY: help check preview dev-init db-up db-down db-logs migrate bootstrap server worker worker-once fmt lint test test-unit test-db build

# rustup installs into ~/.cargo/bin, which is not on PATH for a non-login `make` shell.
# Adding it here (relative to $HOME, never an absolute personal path) means these targets
# work without editing a shell profile. This export covers the scripts under scripts/,
# which call cargo themselves.
export PATH := $(HOME)/.cargo/bin:$(PATH)

# ...but the exported PATH is not enough on its own: GNU Make 3.81 (the make shipped with
# macOS) resolves a recipe's program itself when the line needs no shell, and it does that
# against the PATH make was *started* with — so a bare `cargo` line still fails. Resolving
# it to a full path here is what actually makes `make lint` work on a stock macOS shell.
# A cargo already on PATH wins; CARGO from the environment or the command line wins over
# both.
ifeq ($(origin CARGO),undefined)
CARGO := $(shell command -v cargo 2>/dev/null || printf '%s' '$(HOME)/.cargo/bin/cargo')
endif

COMPOSE ?= docker compose

help:
	@echo "OTDEL — available targets:"
	@echo ""
	@echo "  Local pilot (phase 1A backend):"
	@echo "    make dev-init     - generate .local credentials/env (password is written to a file, not printed)"
	@echo "    make db-up        - start PostgreSQL (compose project otdel-block1, 127.0.0.1:58432)"
	@echo "    make migrate      - apply migrations with the migration role"
	@echo "    make bootstrap    - provision the configured bureau"
	@echo "    make server       - run the API on 127.0.0.1:18480"
	@echo "    make worker       - run the maintenance worker (no document extraction in 1A)"
	@echo "    make worker-once  - one maintenance pass, then exit"
	@echo "    make db-down      - stop the database container (other projects are untouched)"
	@echo ""
	@echo "  Checks:"
	@echo "    make check        - repository hygiene + cargo fmt/clippy + tests that need no database"
	@echo "    make test-unit    - only the tests that need no database"
	@echo "    make test         - cargo test --workspace (integration tests need the test database env)"
	@echo "    make test-db      - prepare the test database and run the full suite"
	@echo "    make fmt / lint / build"
	@echo ""
	@echo "    make preview      - serve design/ ONLY at http://127.0.0.1:4173 (static prototype)"

# --- local pilot ---------------------------------------------------------------

dev-init:
	./scripts/dev-init.sh

db-up:
	$(COMPOSE) up -d postgres
	@echo "PostgreSQL is starting on 127.0.0.1:58432 (project otdel-block1)."

db-down:
	$(COMPOSE) stop postgres

db-logs:
	$(COMPOSE) logs --tail 50 postgres

migrate:
	./scripts/dev-api.sh migrate

bootstrap:
	./scripts/dev-api.sh bootstrap

server:
	./scripts/dev-api.sh serve

worker:
	./scripts/dev-worker.sh run

worker-once:
	./scripts/dev-worker.sh once

# --- checks --------------------------------------------------------------------

fmt:
	$(CARGO) fmt --all

lint:
	$(CARGO) clippy --workspace --all-targets -- -D warnings

build:
	$(CARGO) build --workspace

test:
	$(CARGO) test --workspace

# Tests that do not need PostgreSQL. The database-backed suites live in
# crates/otdel-api/tests and are skipped here on purpose (they are run by `make test-db`
# and by CI, which provide a real database — they are never faked as passing).
test-unit:
	$(CARGO) test --workspace --lib

# Note the assignment before `eval`: `eval "$(...)"` would swallow a failure of the
# script (an empty eval succeeds), and the suite would then report missing environment
# variables instead of the real provisioning error.
test-db:
	exported="$$(./scripts/dev-test-db.sh --export)" && eval "$$exported" && $(CARGO) test --workspace

check:
	node scripts/check.mjs
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy --workspace --all-targets -- -D warnings
	$(CARGO) test --workspace --lib

# Serves only the design/ directory (static prototype: index.html, prototype.js/css,
# assets/). Binds to 127.0.0.1 (localhost only, not 0.0.0.0) and uses --directory so
# the server root is design/, not the repository root — nothing outside design/ is
# reachable through this server.
preview:
	@test -f design/index.html || { echo "Prototype not present: merge design-system PR first"; exit 1; }
	python3 -m http.server 4173 --bind 127.0.0.1 --directory design
