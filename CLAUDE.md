# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Stalwart is a secure, scalable mail & collaboration server written in Rust, supporting IMAP, JMAP, POP3, SMTP, CalDAV, CardDAV and WebDAV. It is a Cargo workspace (edition 2024) producing a single `stalwart` binary that runs every protocol server, the spam filter, the management HTTP API, and the storage/coordination layers in one process.

Licensed AGPL-3.0-only OR LicenseRef-SEL (dual). Code under SPDX `LicenseRef-SEL` snippets (often guarded by `#[cfg(feature = "enterprise")]`) is the Stalwart Enterprise License — keep enterprise code inside those `SPDX-SnippetBegin/End` markers and `enterprise` feature gates.

## Build, test, run

The binary's storage backends, message broker, and Enterprise features are all selected by Cargo features — there is no single "build everything". Build with `--no-default-features` plus the explicit feature set you need.

```bash
# Local dev build (default features = rocks + enterprise)
cargo build -p stalwart

# Production feature set (matches the Docker/CI release build)
cargo build --release -p stalwart --no-default-features \
  --features "sqlite postgres mysql rocks s3 redis azure nats enterprise"

# Run the server (expects a config dir; see resources/ and docs)
cargo run -p stalwart -- --config /etc/stalwart/config.toml

# Style check (must pass before commits — CI runs this first)
cargo fmt --all --check
```

Backend features fan out from the `main` crate into `store`/`directory`/`coordinator` (e.g. `postgres` → `store/postgres` + `directory/postgres`). When adding a backend-specific code path, gate it on the feature in the leaf crate and wire the feature through `crates/main/Cargo.toml`.

### Tests

Integration tests live in the top-level `tests` crate, organized by subsystem (`jmap`, `imap`, `smtp`, `webdav`, `directory`, `store`, `cluster`, `automation`, `system`, `telemetry`). The default test feature set is `rocks + sqlite`; other backends are opt-in.

```bash
# Protocol-level unit tests (fast, no external services)
cargo test -p jmap_proto -- --nocapture
cargo test -p imap_proto -- --nocapture
cargo test -p store    -- --nocapture   # includes full-text search tests

# Integration tests, filtered by module name
cargo test -p tests directory -- --nocapture
cargo test -p tests smtp      -- --nocapture
cargo test -p tests imap      -- --nocapture

# A single test
cargo test -p tests <test_name> -- --nocapture

# Integration tests against a specific backend
cargo test -p tests --features postgres store -- --nocapture
```

Some integration tests require external services to be running (LDAP via glauth, S3 via MinIO) — see `.github/workflows/test.yml` and `tests/resources/` for how CI provisions them. Tests build with `test_mode` (and usually `enterprise`) features enabled on the crates under test.

## Architecture

### proto vs. logic crate split

Several protocols are split into a parser/codec crate and a server-logic crate. Keep wire-format parsing/serialization in the `*-proto` crate (stateless, no I/O) and protocol semantics + storage interaction in the sibling crate:

- `jmap-proto` / `jmap`
- `imap-proto` / `imap`
- `dav-proto` / `dav`
- `http-proto` / `http`

`smtp-proto` and `mail-parser` are external Stalwart Labs crates (pulled from crates.io, not in this workspace).

### Crate map

**Protocol servers** — `jmap`, `imap`, `pop3`, `smtp`, `managesieve`, `dav` (CalDAV/CardDAV/WebDAV), `http` (management API + JMAP-over-HTTP).

**Domain logic** — `email` (message store, mailboxes, threads), `groupware` (calendars, contacts, file storage), `spam-filter` (rules, statistical classifier, LLM analysis), `nlp` (tokenization, language detection, full-text indexing helpers), `directory` (users/groups/auth backends: internal, LDAP, SQL, OIDC).

**Infrastructure** — `store` (pluggable data/blob/in-memory backends + query/write/search dispatch; see `crates/store/src/{backend,dispatch,query,write,search}`), `coordinator` (cluster coordination & pub/sub: peer-to-peer, NATS, Kafka, Redis, Zenoh), `services` (background tasks, queue manager, broadcast subscriber), `registry` (config schema registry), `migration` (database schema/version migrations, run at startup), `trc` (tracing/telemetry/event collector), `types` (shared core types), `utils`.

**`common`** — the shared runtime hub everything depends on: config loading & macro expansion (`config`, `manager`), authentication (`auth`), caching (`cache`), expression engine (`expr`), networking & TLS/ACME (`network`), sharing/ACLs (`sharing`), Sieve scripting host (`scripts`), storage abstractions (`storage`), i18n (`i18n`), inter-process channels (`ipc`), and the `enterprise` module. Most crates take a built `Server` object originating here.

**`main`** — the binary. `crates/main/src/main.rs` is the entry point: installs the aws-lc-rs Rustls provider, boots config via `BootManager::init()`, runs `migration::try_migrate`, starts services + queue manager, then spawns one session manager per configured protocol listener.

### Startup flow

`main()` → `BootManager::init()` (load config) → `migration::try_migrate` → `start_services()` + `start_queue_manager()` → log config errors/warnings → `servers.spawn(...)` matches each listener's `ServerProtocol` to its `SessionManager` (`SmtpSessionManager`, `ImapSessionManager`, `Pop3SessionManager`, `ManageSieveSessionManager`, `HttpSessionManager`) → `wait_for_shutdown()`.

## Conventions

- **Edition 2024**, `resolver = "2"`. No `rust-toolchain.toml` is pinned.
- **Allocator**: jemalloc (`jemallocator`) is the global allocator on non-MSVC targets; MSVC uses the system allocator.
- **TLS/crypto**: Rustls with the `aws-lc-rs` provider, installed explicitly at startup. Don't introduce OpenSSL.
- **Lints**: `crates/main` warns on `clippy::large_futures`, `cast_possible_truncation`, `cast_possible_wrap`, `cast_sign_loss`. Be deliberate about boxing large async futures and about numeric casts.
- **SPDX headers**: every source file carries an SPDX copyright + license header; preserve it on new files.
- **Release profile** uses `lto = true`, `codegen-units = 1`, `strip = true` — release builds are slow; prefer `cargo build -p stalwart` (dev, incremental) while iterating and the protocol `*-proto` test crates for quick feedback.

## Resources & config

- `resources/` — config schema (`schema/schema.json.gz`, surfaced via the `registry` crate), HTML templates, locales, systemd units, apparmor profiles, helper scripts.
- `api/v1/openapi.yml` — the management HTTP API spec.
- `UPGRADING/` — per-version migration notes (`v0_16.md`, etc.); consult when touching `migration` or changing on-disk/schema formats.
- `install.sh`, `Dockerfile`, `Dockerfile.build`, `Dockerfile.fdb`, `docker-bake.hcl` — packaging. CI release builds run via `docker buildx bake`.

## Contributing note

This repo (upstream Stalwart) only accepts bug fixes and small, well-scoped changes pre-1.0; the architecture is still evolving. Contributions are AGPL-3.0 and require signing the Fiduciary License Agreement. Keep changes minimal and focused.
