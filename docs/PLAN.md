# Implementation plan

Unchecked milestones are not implemented. Consult [SPEC.md](SPEC.md) for complete scope and [TESTING.md](TESTING.md) for acceptance checks.

## 0. Project preparation

- [x] Preserve the original brief.
- [x] Establish agent instructions, architecture, testing guidance, and repository defaults.
- [x] Create a single Cargo package with a thin `curly` binary and generate `Cargo.lock`.
- [x] Select dependency versions/features and establish formatting, Clippy, and test CI configuration.

## 1. CLI and executor

- [x] URL-first parsing; no-argument help; explicit `tui`, saved-request, and history command paths.
- [x] Method, ordered/repeated headers and query parameters, exclusive JSON/raw body sources, basic/bearer/environment authentication.
- [x] Explicit stdin reading and `@path` files; default GET/POST behavior; early validation.
- [x] Shared asynchronous reqwest/Rustls executor; 10-second connection and 30-second total defaults; configurable/disabled total timeout.
- [x] Opt-in redirects capped at ten, TLS verification and explicit insecure mode, no automatic retries.
- [x] Streaming response interface, timing, cancellation, and error/exit-code mapping.
- [x] Local-server acceptance coverage for parsing, transport, authentication, TLS, redirects, and timeouts.

## 2. Output contract

- [x] Implement `--output`, `--raw`, `--headers`, `--quiet`, `--color`, and `--fail` with documented interactions.
- [x] Exact pipeline/file bytes; stderr separation; HTTP error bodies preserved; quiet broken pipes.
- [x] Terminal JSON formatting/highlighting up to 1 MiB; larger streaming responses; escaped controls and binary summaries.
- [ ] Verify bounded memory, exact bytes, binary downloads, terminal behavior, and cancellation exits.

## 3. Persistence and replay

- [x] Standard directories and environment overrides; configuration schema and invalid-config errors.
- [x] Versioned saved-request TOML; save-and-execute, overwrite protection, list/show/delete.
- [x] Environment-reference preservation, relative body files, stdin save rejection, and literal-secret policy.
- [x] Versioned SQLite migrations, bounded busy timeout, transactional history retention, off-loop persistence.
- [x] Terminal-default history and script opt-in; 64 KiB request/response previews; explicit truncation and 1,000-entry retention.
- [x] Redaction, history list/show/clear, complete-input replay validation, warning-only history-write failures.
- [x] Round-trip, concurrency, migrations, retention, redaction, and missing-input tests.

## 4. TUI

- [x] Interactive-stream requirements and terminal session cleanup through a terminal-session guard.
- [x] Searchable history/details panes with header/body views, scrolling, JSON display highlighting, and truncation indicators.
- [x] Arrows/j/k, Tab, /, r, q; confirmation for methods other than GET/HEAD/OPTIONS.
- [x] Asynchronous replay, loading feedback, cancellation, and recorded replay history.
- [ ] Ratatui state/render tests and manual Windows resize/restoration checks. Cross-platform TUI verification is deferred with Linux/macOS support.

## 5. Releases and documentation

- [ ] Verify formatting, Clippy, and tests in Windows CI. (Workflow configured; a remote CI run is still required.)
- [x] Build and smoke-test the Windows x64 artifact locally.
- [x] Generate the Windows x64 checksum and inspect its runtime dependencies locally.
- [x] Publish accurate install/use, configuration, persistence/privacy, replay, and troubleshooting documentation.
- [ ] Choose a license and add its exact text before distribution; do not assume a license on the owner's behalf.

## Deferred platform support

- [ ] Add Linux x64-musl CI/release packaging after native verification is available.
- [ ] Add macOS Intel and Apple Silicon CI/release packaging after native verification is available.
- [ ] Repeat TUI resize/restoration and runtime-linkage checks on each added platform.

## Deferred beyond v1

Multipart/forms, cookie jars, explicit proxy controls, collections/environments, curl import/export, plugins, and a full TUI request editor.
