-include .deploy.env
SERVER_HOST   ?= user@your-server
SERVER_DIR    ?= ~/opex
SERVER_TARGET := x86_64-unknown-linux-gnu
AUTH          ?= $(shell cat .auth-token 2>/dev/null || echo "MISSING_AUTH_TOKEN")

.PHONY: check test test-gemini test-db test-db-up test-db-down lint audit build build-x86_64 ui release gen-types deploy-binary-server deploy-remote remote-build remote-deploy doctor logs restart status clean llvm-cov

# ── Codegen ──────────────────────────────────────────────────────────────────

gen-types:
	cargo run --features ts-gen --bin gen_ts_types -p opex-core

# ── Development ──────────────────────────────────────────────────────────────

check:
	cargo check --all-targets

test:
	cargo test --features gemini-cloudcode

# Run only the gemini_cloudcode oauth subtree (useful during isolated OAuth development).
test-gemini:
	cargo test -p opex-core --features gemini-cloudcode gemini_cloudcode::

# Coverage report — requires cargo-llvm-cov (`cargo install cargo-llvm-cov`).
# Generates an HTML report and opens it in the default browser.
llvm-cov:
	cargo llvm-cov --features gemini-cloudcode --html --open

# ── DB-backed integration tests (sqlx::test) ──────────────────────────────────
# `test-db-up` boots an isolated Postgres on 127.0.0.1:5434 (separate from the
# dev `postgres` service on 5432). `test-db` runs the full suite against it
# with DATABASE_URL pointed at the test instance — sqlx::test creates one
# ephemeral DB per test and drops it on success.
#
# Why a second instance: production data lives in the dev `postgres` service.
# Running `cargo test` against that container would have sqlx::test try to
# CREATE/DROP databases as the production user, which is destructive in
# practice. The test instance uses tmpfs so per-test DB churn is fast and
# state never survives a docker compose down.

TEST_DB_URL := postgres://opex_test:opex_test@127.0.0.1:5434/opex_test

test-db-up:
	cd docker && docker compose -f docker-compose.test.yml up -d --build postgres-test
	@echo "Waiting for postgres-test to become healthy..."
	@for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do \
		if docker exec docker-postgres-test-1 pg_isready -U opex_test -d opex_test >/dev/null 2>&1; then \
			echo "  postgres-test ready"; break; \
		fi; \
		sleep 1; \
	done

test-db-down:
	cd docker && docker compose -f docker-compose.test.yml down -v

test-db: test-db-up
	DATABASE_URL=$(TEST_DB_URL) cargo test --bin opex-core
	@echo "test-db complete (postgres-test still up; run 'make test-db-down' to clean up)"

lint:
	cargo clippy --all-targets -- -D warnings

# Run RustSec advisory check. Ignore policy lives in `.cargo/audit.toml`
# with per-entry rationale — re-evaluate every release.
audit:
	cargo audit --deny warnings

# ── Build ────────────────────────────────────────────────────────────────────

build:
	cargo build --release

# x86_64 production server build (home-lab box). Same workspace, no OpenSSL —
# all crates pinned to rustls.
build-x86_64:
	cargo zigbuild --release --target $(SERVER_TARGET) -p opex-core -p opex-watchdog -p opex-memory-worker

ui:
	cd ui && npm run build

release:
	bash release.sh --all

# ── Remote-build deploy (canonical) ──────────────────────────────────────────
# Build natively ON the server (i7-8700, 12T, 31GB) — no cross-toolchain.
# Source lives at ~/opex-src on the server; this is the canonical
# workflow now (see project_build_on_server memory). Use deploy-binary-server
# only if you specifically need to build locally and scp the binary.

# Full remote cycle: git pull → cargo build --release → atomic swap → restart.
remote-deploy:
	ssh $(SERVER_HOST) '~/opex-src/scripts/server-deploy.sh'

# Build only (no swap, no restart). Useful for CI-style verification.
# F124: build WITH --features opex-core/gemini-cloudcode to match the real deploy
# (server-deploy.sh) — otherwise the gemini-cloudcode module + its optional deps
# are excluded and a compile error there passes remote-build but fails the deploy.
remote-build:
	ssh $(SERVER_HOST) 'cd ~/opex-src && git pull --ff-only && . ~/.cargo/env && cargo build --release --features opex-core/gemini-cloudcode -p opex-core -p opex-watchdog -p opex-memory-worker'

# Skip rebuild: redeploy from existing target/release on the server.
deploy-remote: remote-deploy

# ── Legacy: local cross-compile + scp deploy ─────────────────────────────────
# x86_64 production server deploy: build locally + scp the binaries. atomic mv
# works around the mmap'd-binary scp overwrite issue (see fix(deploy) commit).
# Prefer `make remote-deploy` for the normal workflow; use this only when a
# push-to-remote build is undesired or the server is busy.
deploy-binary-server: build-x86_64
	@for PAIR in opex-core:opex-core opex-watchdog:opex-watchdog opex-memory-worker:opex-memory-worker; do \
		CRATE=$${PAIR%%:*}; RUN=$${PAIR##*:}; \
		BIN=target/$(SERVER_TARGET)/release/$$CRATE; \
		if [ -f "$$BIN" ]; then \
			scp $$BIN $(SERVER_HOST):$(SERVER_DIR)/$${RUN}-x86_64.new && \
			ssh $(SERVER_HOST) "mv -f $(SERVER_DIR)/$${RUN}-x86_64.new $(SERVER_DIR)/$${RUN}-x86_64" && \
			echo "  deployed $$CRATE -> $${RUN}-x86_64"; \
		fi; \
	done
	ssh $(SERVER_HOST) "chmod +x $(SERVER_DIR)/opex-*-x86_64; for SVC in opex-core opex-watchdog opex-memory-worker; do systemctl --user is-enabled \$$SVC 2>/dev/null && systemctl --user restart \$$SVC && echo \"  restarted \$$SVC\" || true; done"

# ── Remote ops ───────────────────────────────────────────────────────────────
# Operational targets run against the live SERVER_HOST.

doctor:
	@ssh $(SERVER_HOST) "curl -sf -H 'Authorization: Bearer $(AUTH)' http://localhost:18789/api/doctor | python3 -m json.tool"

logs:
	ssh $(SERVER_HOST) "journalctl --user -u opex-core -f --no-pager"

restart:
	ssh $(SERVER_HOST) "systemctl --user restart opex-core"

status:
	ssh $(SERVER_HOST) "systemctl --user status opex-core --no-pager"

# ── Cleanup ──────────────────────────────────────────────────────────────────

clean:
	cargo clean
	rm -rf ui/out ui/.next
