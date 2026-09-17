# cURLy agent instructions

## Project

Build `curly`, a Rust HTTP client for personal JSON API workflows. Ship one executable per supported target, with no server, account, or separately installed application libraries. The CLI is primary; the optional history TUI launches only through `curly tui`.

Read [docs/SPEC.md](docs/SPEC.md) before implementation. It preserves the original product requirements. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md), [docs/PLAN.md](docs/PLAN.md), and [docs/TESTING.md](docs/TESTING.md) for design, sequencing, and acceptance checks. If these documents disagree, follow the user's current instructions, then the original specification. Resolve substantive ambiguity explicitly rather than silently weakening a requirement.

## Scope and sequence

Implement in this order:

1. CLI and shared request executor.
2. Output and pipeline contract.
3. Configuration, saved requests, history, and replay.
4. TUI.
5. Release packaging and documentation.

Finish and verify each milestone before expanding scope. Defer multipart/forms, cookie jars, explicit proxy controls, collections/environments, curl import/export, plugins, and request editing in the TUI.

## Structure and dependencies

- Use one Cargo package named `curly`, a thin `src/main.rs`, and internal modules for CLI parsing, request modeling, execution, output, storage, and TUI. A `src/lib.rs` may expose those modules to integration tests within the same package.
- Use Tokio, reqwest with Rustls, clap, serde/serde_json, TOML, Ratatui/Crossterm, and rusqlite with bundled SQLite. Review dependency feature flags to avoid accidentally introducing native TLS or external SQLite requirements.
- Generate and commit `Cargo.lock` when creating the package. Do not fabricate a lockfile or claim a minimum supported Rust version without testing it.
- Keep the request model independent of clap and TUI types. Preserve ordered, duplicate headers and query parameters.
- Both interfaces must use the same asynchronous executor. Keep blocking persistence work off the TUI event loop.

## Behavioral invariants

- Accept only HTTP/HTTPS URLs. Default to GET, or POST when a body is supplied unless the method is explicit. Read stdin only for an explicit `-`; `@path` denotes a body file. `--json` and `--body` are mutually exclusive.
- Verify TLS by default. Require `--insecure` to disable verification. Default connection timeout: 10 seconds; total timeout: 30 seconds, with an explicit way to disable the latter. Follow redirects only with `--follow`, at most ten hops. Never retry automatically.
- Detect stdout with `std::io::IsTerminal`. Pipes/files receive exact response-body bytes, with no formatting or appended newline. Diagnostics and requested headers use stderr. Preserve error bodies with `--fail`; handle downstream broken pipes quietly.
- For terminal display, format/highlight valid JSON only up to 1 MiB. Larger responses stream. Escape terminal controls in displayed text and show binary summaries. Never sanitize or decorate raw pipeline/download bytes.
- Exit codes: 0 success; 1 transport/local I/O error; 2 invalid arguments/configuration; 22 HTTP 4xx/5xx with `--fail`; 130 cancellation. History-write failures warn without replacing an otherwise successful HTTP result.
- Stream responses with bounded memory; retain at most 64 KiB each of request/response history preview with explicit truncation markers. Keep only the newest 1,000 history entries.
- Enable history by default only for terminal stdout; support explicit `--history` and `--no-history` overrides.

## Persistence and credentials

- Use platform-standard configuration and local data directories, honoring `CURLY_CONFIG_DIR` and `CURLY_DATA_DIR`.
- Save a versioned TOML file per named request. `--save NAME` saves and executes; reject an existing name unless `--overwrite` is supplied. Keep request names inside the designated storage directory.
- Preserve environment references; do not persist credentials resolved from them. Resolve saved body-file paths relative to their request TOML file. Reject saving stdin-backed requests with actionable guidance.
- Redact authorization, proxy authorization, cookie, and set-cookie header values before history persistence, case-insensitively. Do not persist literal authentication secrets through a parallel structured-auth field. Make missing replay inputs explicit; never send redaction placeholders as credentials.
- Replay only complete requests whose bodies and credentials are available. Previews are not automatically replayable bodies. Document that URLs and body previews may contain sensitive data.
- Use versioned SQLite migrations, transactions, and a bounded busy timeout. Keep external calls out of database transactions.

## TUI

Require interactive stdin and stdout. Provide searchable history beside request/response details, header/body views, scrolling, JSON highlighting, and visible truncation. Support arrows/j/k, Tab, /, r, and q. Confirm replay of methods other than GET, HEAD, and OPTIONS. Include asynchronous loading, cancellation, and history recording for replays. Restore terminal state after normal exit, errors, and panic.

## Verification and completion

- Use local test servers and temporary storage directories. Do not depend on public APIs or real credentials in automated tests.
- Add tests for meaningful behavior and failure modes using [docs/TESTING.md](docs/TESTING.md). Check exact bytes and exit codes for pipeline behavior.
- Once Cargo scaffolding exists, run `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets --all-features -- -D warnings`, and `cargo test --locked --all-targets --all-features` for implementation milestones.
- Release and verify Windows x64 only for now. Linux and macOS release support is deferred until those platforms can be properly built, smoke-tested, and dependency-inspected. Do not re-add unverified platform release targets.
- Keep docs and milestone status accurate. Report what changed, what was verified, and material limitations. Do not claim unrun tests or unbuilt releases passed.
- After any change that affects the executable, once the Windows release build and required checks succeed, automatically copy the verified binary to `%USERPROFILE%\.local\bin\curly.exe`. This directory is already on the user PATH. Before finishing, verify from a fresh shell that `where curly` resolves to that file and `curly --version` succeeds; do not wait for the user to ask for PATH installation again.
