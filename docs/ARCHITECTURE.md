# Architecture

This document describes the implemented module boundaries and the design constraints that remain authoritative. The full product requirements are preserved in [SPEC.md](SPEC.md).

## Package layout

The repository uses one Cargo package, `curly`, with a thin binary and a library entry point used by the binary and tests. Module boundaries:

| Module | Responsibility |
| --- | --- |
| `cli` | clap definitions, command dispatch inputs, argument validation |
| `request` | Interface-independent request definitions, body sources, authentication, transport settings |
| `executor` | reqwest/Rustls HTTP execution, redirect limits, timing, streaming, cancellation |
| `output` | Byte sinks, terminal formatting, JSON highlighting, control escaping, stderr diagnostics |
| `storage::config` | Platform directory resolution, configuration loading and validation |
| `storage::saved` | Versioned TOML definitions, names, overwrite rules, relative body-file resolution |
| `storage::history` | SQLite migrations, redaction, bounded previews, retention, replay eligibility |
| `tui` | Request composer, history workspace, event handling, rendering, asynchronous request coordination |

Use Tokio for asynchronous work, clap for CLI parsing, serde/serde_json and TOML for serialization, Ratatui/Crossterm for the terminal UI, and rusqlite with bundled SQLite. Dependency versions and feature selections will be recorded in Cargo metadata and its generated lockfile during scaffolding.

## Request lifecycle

1. Parse CLI input, build a request in the TUI composer, or load a saved/history definition.
2. Validate URL, method, exclusive body/auth options, and transport settings into a shared request model.
3. Resolve environment references and body sources for execution, retaining the unresolved definition separately for safe persistence.
4. Execute through a single shared executor. Pass cancellation through to the active operation.
5. Stream response chunks to an output consumer while accumulating byte counts and bounded previews.
6. Persist a redacted history record when enabled; report persistence failures separately from HTTP success.
7. Map the result to the documented exit code or TUI state.

Represent headers and query parameters as ordered sequences, not maps. Model empty/inline/file/stdin body sources and literal/environment authentication distinctly. A body source's presence, including an empty body, must be considered when choosing the default method.

## Output and memory

Output decisions depend on destination and explicit flags. Use `IsTerminal` rather than guessing from environment variables. Pipes and output files receive unchanged body bytes. All status/timing, errors, and requested response headers use stderr.

The terminal JSON formatter may buffer at most its 1 MiB candidate window plus a bounded chunk to detect overflow. Once the limit is crossed, flush the buffered prefix through the terminal-safe streaming renderer and continue without formatting. Avoid buffering the entire response just to decide how to display it. Binary terminal responses use a summary. History previews have independent 64 KiB caps for request and response, regardless of destination.

Define `--raw`, `--quiet`, color precedence, binary detection, and broken-pipe completion behavior in CLI help and focused tests during the output milestone. Terminal safety must be tested across chunk boundaries, including split escape sequences and UTF-8 sequences. Download/pipeline bytes must remain untouched.

## Local storage

Use a platform-directory library after verifying the directory behavior on supported systems. `CURLY_CONFIG_DIR` replaces the configuration root; `CURLY_DATA_DIR` replaces the local data root. Proposed contents:

```text
<config root>/
  config.toml
  requests/
    <name>.toml
<data root>/
  history.sqlite3
```

Each named request includes a schema version. Validate names to prevent path traversal and apply an explicit overwrite policy. Write complete definitions atomically where supported. Body file paths are resolved relative to the containing request file; saving a CLI-relative path must preserve the intended file when its base changes. Stdin-backed definitions cannot be saved.

SQLite uses numbered migrations, transactions, and a bounded busy timeout. Store timestamps, request details, status/error, elapsed time, headers, byte counts, and previews with truncation metadata. Insert and prune to the newest 1,000 entries transactionally using a deterministic ordering. Use blocking workers or a dedicated persistence worker so SQLite cannot stall the TUI event loop.

## Redaction and replay

Redact sensitive header values before serialization, including case variants. Keep raw credentials out of metadata, errors, and structured authentication fields used for history. Environment references remain references; resolve their values only at execution time.

Record enough metadata to distinguish complete replay inputs from previews, truncated bodies, and redacted credentials. Do not treat a redaction marker as a usable input. Missing files, missing environment variables, missing literal credentials, and incomplete bodies must produce actionable errors before sending a request. Define and document the saved-request policy for explicitly supplied literal secrets before enabling saved authentication.

The TUI composer creates the same `RequestDefinition` used by the CLI and executes it through the same executor. Completed TUI requests are recorded in history. TUI replay confirms methods other than GET, HEAD, and OPTIONS, and replayable history requests can be loaded back into the composer for editing before an explicit send.

## Terminal lifecycle and releases

Use an explicit terminal-session guard and panic handling to restore raw mode, cursor visibility, and the alternate screen. Structure the TUI around testable state transitions and asynchronous completion events, including loading and cancellation.

Release packaging currently targets only `x86_64-pc-windows-msvc`, which has been locally built, smoke-tested, and dependency-inspected. Linux and macOS packaging are deferred until those targets can be verified on their native platforms.

TUI mouse hit targets are registered from the rectangles actually rendered, including the current history list offset. Modal editors, help, replay confirmation, and loading expose only their own actions. Clicks share keyboard action handlers. `tui::preview` builds terminal-safe, bounded JSON/HTML/XML/header text, caches wrapping by pane width, and renders only the visible lines. Source response bytes and stored previews are unchanged. Mouse capture is paired with terminal-session cleanup, including setup failure.

The TUI shell owns its full background palette and uses a full-width endpoint bar above asymmetric request/response panes. Request sections collapse to compact controls on small terminals. Response documents cache wrapping at the content width (excluding the line-number gutter), and rendering clones only the visible rows. Status metrics and truncation remain outside the scrolling response content.

`tui::editor` maintains temporary ordered name/value rows for headers and query parameters, commits only validated drafts, and retains structured pairs instead of reconstructing them from display delimiters. JSON editing validates before commit and request construction; body-file I/O remains in the executor. Formatting and starter templates are explicit actions.
