-include .deploy.env
PI_HOST   ?= user@your-server
PI_DIR    := ~/hydeclaw
TARGET    := aarch64-unknown-linux-gnu
BIN       := target/$(TARGET)/release/hydeclaw-core
AUTH      ?= $(shell cat .auth-token 2>/dev/null || echo "MISSING_AUTH_TOKEN")

.PHONY: check test test-db test-db-up test-db-down build build-arm64 build-arm64-otel ui release gen-types deploy-binary deploy-binary-otel deploy-ui deploy-migrations deploy-prompts deploy deploy-docker deploy-jaeger jaeger-up jaeger-down doctor clean

# ── Codegen ──────────────────────────────────────────────────────────────────

gen-types:
	cargo run --features ts-gen --bin gen_ts_types -p hydeclaw-core

# ── Development ──────────────────────────────────────────────────────────────

check:
	cargo check --all-targets

test:
	cargo test

# ── DB-backed integration tests (sqlx::test) ──────────────────────────────────
# `test-db-up` boots an isolated Postgres on 127.0.0.1:5433 (separate from the
# dev `postgres` service on 5432). `test-db` runs the full suite against it
# with DATABASE_URL pointed at the test instance — sqlx::test creates one
# ephemeral DB per test and drops it on success.
#
# Why a second instance: production data lives in the dev `postgres` service.
# Running `cargo test` against that container would have sqlx::test try to
# CREATE/DROP databases as the production user, which is destructive in
# practice. The test instance uses tmpfs so per-test DB churn is fast and
# state never survives a docker compose down.

TEST_DB_URL := postgres://hydeclaw_test:hydeclaw_test@127.0.0.1:5434/hydeclaw_test

test-db-up:
	cd docker && docker compose -f docker-compose.test.yml up -d --build postgres-test
	@echo "Waiting for postgres-test to become healthy..."
	@for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do \
		if docker exec docker-postgres-test-1 pg_isready -U hydeclaw_test -d hydeclaw_test >/dev/null 2>&1; then \
			echo "  postgres-test ready"; break; \
		fi; \
		sleep 1; \
	done

test-db-down:
	cd docker && docker compose -f docker-compose.test.yml down -v

test-db: test-db-up
	DATABASE_URL=$(TEST_DB_URL) cargo test --bin hydeclaw-core
	@echo "test-db complete (postgres-test still up; run 'make test-db-down' to clean up)"

lint:
	cargo clippy --all-targets -- -D warnings

# ── Build ────────────────────────────────────────────────────────────────────

build:
	cargo build --release

build-arm64:
	cargo zigbuild --release --target $(TARGET) -p hydeclaw-core -p hydeclaw-watchdog -p hydeclaw-memory-worker

# OTel-enabled binary for Pi. Adds OTLP exporter dependency (~3 MB). Use
# together with `make deploy-jaeger` and `[otel] enabled = true` in
# hydeclaw.toml. Worker + watchdog stay on the default feature set —
# they don't have hot paths worth tracing yet.
build-arm64-otel:
	cargo zigbuild --release --target $(TARGET) -p hydeclaw-core --features otel
	cargo zigbuild --release --target $(TARGET) -p hydeclaw-watchdog -p hydeclaw-memory-worker

ui:
	cd ui && npm run build

release:
	bash release.sh --all

# ── Deploy to Pi ─────────────────────────────────────────────────────────────

deploy-binary: build-arm64
	@for CRATE in hydeclaw-core hydeclaw-watchdog hydeclaw-memory-worker; do \
		BIN=target/$(TARGET)/release/$$CRATE; \
		if [ -f "$$BIN" ]; then \
			scp $$BIN $(PI_HOST):$(PI_DIR)/$${CRATE}-aarch64; \
			echo "  deployed $$CRATE"; \
		fi; \
	done
	ssh $(PI_HOST) "chmod +x $(PI_DIR)/hydeclaw-*-aarch64; for SVC in hydeclaw-core hydeclaw-watchdog hydeclaw-memory-worker; do systemctl --user is-enabled \$$SVC 2>/dev/null && systemctl --user restart \$$SVC && echo \"  restarted \$$SVC\" || true; done"

# OTel-instrumented binary deploy. Pair with `make deploy-jaeger` and set
# `[otel] enabled = true` in hydeclaw.toml on Pi. Core service must be
# restarted with OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4317 (default).
deploy-binary-otel: build-arm64-otel
	@for CRATE in hydeclaw-core hydeclaw-watchdog hydeclaw-memory-worker; do \
		BIN=target/$(TARGET)/release/$$CRATE; \
		if [ -f "$$BIN" ]; then \
			scp $$BIN $(PI_HOST):$(PI_DIR)/$${CRATE}-aarch64; \
			echo "  deployed $$CRATE (otel)"; \
		fi; \
	done
	ssh $(PI_HOST) "chmod +x $(PI_DIR)/hydeclaw-*-aarch64; systemctl --user restart hydeclaw-core; echo '  restarted hydeclaw-core (otel build)'"

deploy-ui: ui
	ssh $(PI_HOST) "rm -rf $(PI_DIR)/ui/out"
	cd ui && tar cf - out | ssh $(PI_HOST) "mkdir -p $(PI_DIR)/ui && cd $(PI_DIR)/ui && tar xf -"

deploy-migrations:
	scp migrations/*.sql $(PI_HOST):$(PI_DIR)/migrations/

# Channel formatting prompts — read at startup by `channels/src/formatting.ts`
# to populate per-channel system-prompt augmentation. They live under
# `workspace/` (which is intentionally a writable agent-state directory and
# therefore not part of the binary deploy), but these specific files are
# code-owned (tracked in git, edited by developers, not the agent). Sync
# them on every `deploy` so the channels Bun process can find them.
deploy-prompts:
	ssh $(PI_HOST) "mkdir -p $(PI_DIR)/workspace/prompts/formatting"
	scp workspace/prompts/formatting/*.md $(PI_HOST):$(PI_DIR)/workspace/prompts/formatting/

deploy-docker:
	@echo "Syncing docker/ source to Pi (excludes workspace files)..."
	rsync -av --delete \
		--exclude '__pycache__' --exclude '*.pyc' --exclude 'node_modules' \
		docker/ $(PI_HOST):$(PI_DIR)/docker/
	ssh $(PI_HOST) "cd $(PI_DIR)/docker && docker compose up -d --build"

deploy: deploy-binary deploy-ui deploy-migrations deploy-prompts deploy-docker
	@echo "Full deploy complete. Checking health..."
	@sleep 5
	@ssh $(PI_HOST) "curl -sf -H 'Authorization: Bearer $(AUTH)' http://localhost:18789/api/doctor | python3 -m json.tool"

# ── Observability ────────────────────────────────────────────────────────────
# `jaeger-up` boots Jaeger all-in-one on Pi (OTLP receiver + UI). Pair with
# `make deploy-binary-otel` and `[otel] enabled = true` in hydeclaw.toml.
# UI: ssh tunnel `ssh -L 16686:127.0.0.1:16686 $(PI_HOST)`, then open
# http://localhost:16686.

jaeger-up:
	scp docker/docker-compose.observability.yml $(PI_HOST):$(PI_DIR)/docker/
	ssh $(PI_HOST) "cd $(PI_DIR)/docker && docker compose -f docker-compose.observability.yml up -d"
	@echo "Jaeger UI: ssh -L 16686:127.0.0.1:16686 $(PI_HOST)  →  http://localhost:16686"

jaeger-down:
	ssh $(PI_HOST) "cd $(PI_DIR)/docker && docker compose -f docker-compose.observability.yml down"

# Convenience: full observability rollout — binary + jaeger + restart.
deploy-jaeger: jaeger-up deploy-binary-otel
	@echo "Observability deploy complete. Tail spans with jaeger UI."

# ── Remote ───────────────────────────────────────────────────────────────────

doctor:
	@ssh $(PI_HOST) "curl -sf -H 'Authorization: Bearer $(AUTH)' http://localhost:18789/api/doctor | python3 -m json.tool"

logs:
	ssh $(PI_HOST) "journalctl --user -u hydeclaw-core -f --no-pager"

restart:
	ssh $(PI_HOST) "systemctl --user restart hydeclaw-core"

status:
	ssh $(PI_HOST) "systemctl --user status hydeclaw-core --no-pager"

# ── Cleanup ──────────────────────────────────────────────────────────────────

clean:
	cargo clean
	rm -rf ui/out ui/.next
