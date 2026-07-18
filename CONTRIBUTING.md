> **Language:** English · [Русский](CONTRIBUTING.ru.md)

# Contributing to OPEX

Thanks for your interest in the project! Here's how to get started.

## Getting Started

1. Fork the repository
2. Clone your fork: `git clone https://github.com/AronMav/opex`
3. Create a branch: `git checkout -b feature/your-feature-name`

## Setting Up a Development Environment

### Prerequisites

- Rust 1.85+ (`rustup update stable`)
- PostgreSQL 17 with the pgvector extension
- Bun 1.x (for the channel adapters)
- Python 3.11+ with uv (for toolgate)

### Running Locally

```bash
# 1. Start PostgreSQL
docker compose -f docker/docker-compose.yml up -d postgres

# 2. Configure the environment
cp .env.example .env
# Edit .env with your values

# 3. Build and run
cargo run -p opex-core

# 4. (Optional) Start the channel adapters
cd channels && bun install && bun run src/index.ts
```

### Running Tests

```bash
# All tests
make test

# A single test
cargo test test_name -- --nocapture

# UI tests
cd ui && npm test

# Channel adapter tests
cd channels && bun test
```

### Linting

```bash
make lint          # cargo clippy --all-targets -- -D warnings
cd ui && npm run typecheck
```

## Code Style

### Rust

- Follow standard Rust idioms (`cargo clippy` must pass with `-D warnings`)
- Use `anyhow` for error propagation in application code, `thiserror` for library errors
- No `unwrap()` or `expect()` in production paths — use `?` or proper error handling
- All dependencies must use `rustls-tls` (no OpenSSL) to keep cross-compilation working

### TypeScript

- Strict mode is enabled — no `any` types
- Follow the existing patterns in the codebase

### YAML Tools

When adding a new tool to `workspace/tools/`:

- `description` must be in English and clearly explain when to use the tool
- Set `status: draft` until tested, `status: verified` once confirmed working
- Test all parameters before submitting

## Submitting a Pull Request

1. Make sure tests pass: `make test && make lint`
2. Keep the PR focused — one feature or fix per PR
3. Write a clear PR description explaining what and why
4. Reference any related issues

## Reporting Bugs

When reporting a bug, please include:

- The OPEX version or commit hash
- Your operating system and architecture
- Relevant logs (from `journalctl` or stdout)
- Steps to reproduce

## Security Vulnerabilities

Please **do not** open public issues for security vulnerabilities. Instead, file a [GitHub Security Advisory](https://github.com/AronMav/opex/security/advisories/new) or contact the maintainers directly.

## Creating a Release

```bash
# Build the release archive (all platforms)
./release.sh 0.27.0 --all

# Output: release/opex-v0.27.0.tar.gz
```

The release script syncs the version across `Cargo.toml` and the `package.json` files, builds all binaries, bundles the UI, and produces a single archive.

To publish a release on GitHub, create and push a tag — CI builds and publishes automatically:

```bash
git tag v0.27.0
git push origin v0.27.0
```

## Questions

Open a [GitHub Discussion](https://github.com/AronMav/opex/discussions) for questions about usage, architecture, or design decisions.
