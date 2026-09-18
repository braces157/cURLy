# Testing and acceptance

This document defines the verification plan and records the coverage implemented in the repository.

## Automated checks

After Cargo scaffolding, run:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
```

Use local HTTP/TLS test servers on ephemeral ports, temporary configuration/data directories, deterministic fixtures, and disposable credentials. Isolate environment overrides across tests; prefer child-process environments over mutating shared process state. Assert the absence of unwanted requests when validation or replay eligibility fails.

The current automated suite includes unit tests for request defaults/validation, cancellation mapping, terminal escaping, saved-request safety/round trips, history redaction/replay/retention/concurrency/migration handling, and Ratatui rendering/state. Subprocess integration tests use local HTTP/TLS servers to verify exact piped bytes, `--fail`, redirect opt-in, ordered query values, default POST behavior, timeouts, certificate verification and `--insecure`, basic/bearer authentication, output-file/stderr separation, saved requests, history replay, no-argument help, and warning-only history persistence failures.

## Current local verification

On Windows x64, the required formatting, strict Clippy, and full test commands pass with 41 library tests and 12 subprocess integration tests. The optimized `x86_64-pc-windows-msvc` executable also builds and reports `curly 0.1.0`. Its import table was inspected with `dumpbin`; only Windows system DLLs are referenced. A ZIP and SHA-256 checksum are generated under `dist/`.

Linux and macOS release packaging is currently out of the supported release matrix and will be added later after native verification is available. PTY-driven terminal formatting checks and manual TUI resize/restoration checks remain outstanding for Windows.

| Area | Required cases |
| --- | --- |
| CLI | URL-first commands, no-argument help, HTTP/HTTPS validation, default/explicit methods, conflicting body/auth/history flags, repeatable ordered headers/query, invalid timeout/config values |
| Bodies | Inline JSON/raw, empty body, `@path`, explicit stdin `-`, no implicit stdin reads, missing/unreadable files, malformed JSON, mutually exclusive sources |
| Authentication | Basic, literal bearer, environment bearer, missing variables, references preserved without resolved-secret persistence |
| Transport | Redirects disabled by default, opt-in redirect chain and ten-hop cap, connect/total timeouts and disabled total timeout, TLS failure/insecure opt-in, no automatic retries |
| Errors | Local I/O and transport exit 1, argument/config exit 2, HTTP failures with and without `--fail`, exit 22 with body preserved, cancellation exit 130 |
| Pipelines | Byte-for-byte stdout with no extra newline; stderr headers/diagnostics; output files; binary downloads; quiet/empty responses; quiet downstream broken pipes |
| Terminal | JSON at/below/above 1 MiB, invalid JSON, color modes, raw/quiet/headers interactions, binary summaries, split control and UTF-8 sequences, safe status/header/body/error display |
| Memory | Increasing response sizes with a fixed bounded buffer strategy; streamed byte counts; bounded request and response previews; no full-response collection |
| Saved requests | Versioned TOML round trips, unknown versions, save-and-execute, overwrite rejection/permission, list/show/delete, safe names, relative file resolution, stdin save rejection |
| History | Default terminal behavior and explicit overrides, full metadata, 64 KiB preview boundaries/truncation, newest 1,000 retention, redaction in headers and auth structures, list/show/clear |
| SQLite | Fresh schema, migrations from supported versions, invalid/newer versions, transaction rollback, bounded lock contention, concurrent writes, warning-only write failures |
| Replay | Complete requests, preserved environment refs, missing variables/files/credentials, truncated/unavailable bodies, no redaction placeholders sent, replay history records |
| TUI | Composer parsing/order preservation, request/history workspace rendering, search, focus, navigation, views, scrolling, empty/error states, truncation, history-to-composer loading, confirmation methods, loading/cancellation, background persistence |

Test observable contracts rather than duplicating implementation details. Use controllable delayed/chunked responses for timeout/cancellation coverage; avoid flaky timing thresholds. Distinguish ordinary non-terminal subprocess tests from PTY-driven terminal tests. Document any platform-specific test limitations.

## Manual platform checks

On Windows, verify resizing, composer editing/paste/send, response scrolling/header switching, history loading/filtering, replay confirmation, loading, cancellation, and terminal restoration after success, errors, and panic. Confirm that noninteractive `curly tui` is rejected with actionable guidance. Repeat these checks when Linux or macOS support is added later.

Run a large download through a file sink and a downstream consumer that closes early. Check exact downloaded bytes and that diagnostics do not corrupt the body. Measure memory with increasing input sizes and record methodology rather than asserting bounded memory from a small fixture alone.

## Release checks

| Artifact target | Required evidence |
| --- | --- |
| Windows x64 / MSVC | Executable smoke test and imported-library inspection |

Build from the committed lockfile, attach SHA-256 checksums, and verify the packaged executable launches without separately installed application libraries. Document target-dependent system library requirements and any unverified platform rather than claiming universal static linking.

TUI usability coverage includes Unicode cursor insertion/deletion, edit cancellation, multiline editing, Tab commit/advance, ignoring key-release events, and editor/help rendering at 120×32, 80×24, 48×16, and 20×8. These are TestBackend checks; native interactive Windows visual/resize/restoration checks remain outstanding.

Mouse/preview tests cover click targets at multiple sizes, modal isolation, history offsets, wheel routing, syntax colors, quoted HTML delimiters and raw script text, malformed/truncated Unicode, terminal controls, binary summaries, resize wrapping, and scroll bounds. Native Windows Terminal click/wheel and mouse-capture restoration still require manual verification.

The application-shell redesign was visually inspected using a 140×42 TestBackend render (not a native terminal screenshot). Responsive mouse tests also verify every composer field stays reachable and Send remains aligned with the URL bar at 120×32, 80×24, and 48×18. Existing scrolling/highlighting and Unicode editor checks cover the new layout.

Guided editor tests cover duplicate query names and literal delimiters across save/reopen/cancel, empty values, invalid row recovery/removal, URL and JSON validation, JSON formatting/newline indentation, explicit GET with a body, non-destructive JSON starters, and row-editor scrolling/click targets at three viewport sizes.
