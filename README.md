# cURLy

`curly` is a single-binary Rust HTTP client for personal JSON API workflows. The CLI is the primary automation interface; `curly tui` opens a keyboard-first request workspace with response inspection plus history/replay.

The current implementation includes the CLI/executor, exact pipeline output, saved requests, SQLite history/replay, and the TUI. The currently supported and verified release target is Windows x64; Linux and macOS packaging are deferred until they can be properly verified.

## Build

Use stable Rust with the committed lockfile:

```sh
cargo build --release --locked
```

For development:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
```

Windows x64 release builds enable static CRT linking in the release workflow. TLS uses Rustls and SQLite is bundled, so the executable does not require OpenSSL or a separately installed SQLite library.

## Quick start

```sh
curly https://api.example.com/items
curly https://api.example.com/items -X POST --json '{"name":"example"}'
curly https://api.example.com/items --json @request.json
curly https://api.example.com/items --body -
curly https://api.example.com/items --query limit=10 --bearer-env API_TOKEN
curly https://api.example.com/items --save list-items
curly run list-items
curly history list
curly history show 42
curly history replay 42
curly tui
```

Supplying a body defaults the method to `POST`; otherwise it defaults to `GET`. Only HTTP and HTTPS URLs are accepted. `--json` and `--body` are mutually exclusive. `-` reads the body from stdin only when explicitly requested, while `@path` reads a file.

Repeated `-H/--header` and `--query` values preserve their original order and duplicates. Authentication supports `--basic USER:PASSWORD`, `--bearer TOKEN`, and `--bearer-env VARIABLE`.

## Transport and output

TLS verification is on by default. `--insecure` disables certificate verification explicitly. Redirects are disabled unless `--follow` is supplied and are capped at ten hops. Requests are never automatically retried.

The default connection timeout is 10 seconds and total timeout is 30 seconds. Use `--connect-timeout SECONDS` and `--timeout SECONDS`; `--timeout 0` disables the total timeout.

When stdout is a pipe or redirected file, response body bytes are written exactly as received with no formatting or appended newline. `--output PATH` has the same byte-preserving behavior. Requested response headers and diagnostics go to stderr.

Interactive terminal output pretty-prints valid JSON up to 1 MiB, escapes terminal control characters, and summarizes binary responses. `--raw` disables terminal formatting. `--color auto|always|never`, `--headers`, `--quiet`, and `--fail` control display and error behavior. `--fail` still writes the response body and returns exit code 22 for HTTP 4xx/5xx responses.

## Saved requests

`--save NAME` saves a versioned TOML definition and executes it. Existing names are rejected unless `--overwrite` is supplied.

```sh
curly https://api.example.com/items --bearer-env API_TOKEN --save items
curly run items
curly saved list
curly saved show items
curly saved delete items
```

Saved request names are constrained to the requests directory. File-backed body paths are stored relative to the saved request file when possible. Stdin-backed requests cannot be saved. Literal basic/bearer credentials and credential-bearing headers are deliberately rejected for saved requests; use environment-backed bearer authentication for replayable credentials.

## History and replay

History is enabled by default only when stdout is an interactive terminal. Scripts can opt in with `--history` or disable it with `--no-history`.

```sh
curly history list
curly history show 42
curly history replay 42
curly history clear
```

History uses versioned SQLite migrations, a bounded busy timeout, and keeps the newest 1,000 entries. Request and response previews are capped at 64 KiB each. Authorization, proxy authorization, cookie, and set-cookie values are redacted case-insensitively before persistence.

Replay is allowed only when the original inputs are still complete. Environment references stay as references and must resolve at replay time. Missing files, missing environment variables, redacted literal credentials, stdin bodies, and incomplete body previews are rejected before any request is sent.

URLs, query strings, and body previews may contain sensitive data. History is local but is not encrypted.

## TUI

`curly tui` requires interactive stdin and stdout. It opens on a request composer so you can set a method, URL, ordered headers/query parameters, a JSON/raw body (or `@file`), redirect/TLS options, send the request, and inspect the response without leaving the terminal. TUI requests use the same executor as the CLI and are recorded in the same local history.

Controls:

- `1`/`F1`: request composer; `2`/`F2`: history.
- In Request: `Tab` or `j`/`k` selects a field; `Enter` edits. Click the method to choose from a menu, or use `m` to cycle. `t` switches JSON/raw, `f` toggles redirects, `I` toggles insecure TLS, and `s`/`Ctrl+S` sends.
- Headers and query parameters use **Name / Value rows**. `Tab`/`Enter` moves to the next cell, Shift+Tab moves back, and Up/Down changes rows. **+ Row**/`Ctrl+N` adds; **- Row**/`Ctrl+D` removes. Duplicate names and empty values are supported. Query values are URL-encoded automatically; an encoded preview shows what will be appended. Type literal values without `||` separators or manual percent encoding.
- JSON/raw body editing supports multiline paste, Enter for indented newlines, Tab for two spaces, cursor arrows, Home/End, Backspace/Delete, and `Ctrl+U` to clear. JSON has live syntax coloring and validation with line/column errors. **Format**/`Ctrl+F` pretty-prints valid JSON; **Object** and **Array** create empty starters without replacing existing content. `@path` selects a body file; its contents are checked on send.
- **Save**/`Ctrl+Enter` applies a valid edit; `Esc` discards it. URLs also accept Enter to save. `Ctrl+S` validates, saves, then sends. Invalid URLs, headers, query rows, and JSON remain in the editor with an explanation and cannot be saved/sent. An explicitly selected method is retained when adding a body.
- In Request: `h` shows response headers; `b` shows the body, `PageUp`/`PageDown` scrolls, `n` starts a fresh request, and `Esc` cancels an active send.
- In History: `↑`/`↓` or `j`/`k` navigates, `Tab` switches list/details focus, `/` filters, `h`/`b` switches header/body views, `e` loads a replayable request into the composer, and `r` replays it. Methods other than GET, HEAD, and OPTIONS require replay confirmation.
- `q`: quit.

Mouse controls: click workspace tabs, request fields, and history rows. The visible **Send**, **New**, transport/body-type controls, **Search**, **Edit**, **Replay**, and **Body / Headers** buttons perform the same actions as their keyboard shortcuts. The wheel scrolls the pane under the pointer (or moves history selection). The editor has **Save / Cancel** buttons; active requests have **Cancel**, and unsafe replay has a clickable confirmation. Mouse capture is restored on exit; native terminal text selection may require holding Shift.

Response and history previews pretty-print JSON with colored keys, strings, numbers, and literals. HTML/XML uses a display-only tag layout with colored tag names, attributes, quoted values, and comments. Embedded script/style content remains plain text. Headers have colored names, binary data shows a summary, and truncated previews are marked in the pane title. Formatting/wrapping is cached and scrolling draws only visible lines; previews remain limited to 64 KiB and pipeline/download output is unchanged.

The TUI uses a coordinated charcoal palette with a mint accent, a full-width method/URL/Send bar, separate headers/query/body sections, and a wider response viewer. Secondary controls have subdued styling; the active workspace and Send action are distinct. Response metrics stay above a line-numbered, syntax-colored viewport with a scroll indicator. Narrow terminals switch to compact request controls above the response. URL editing uses a compact dialog; multiline fields keep the larger editor. Idle screens wait for input instead of repeatedly formatting responses. `Ctrl+C` exits while idle and cancels an active request.

The TUI restores raw mode, cursor visibility, and the alternate screen through its terminal-session guard on normal unwinding.

## Storage

Platform-standard configuration and local-data directories are used by default. Override them for portable or test setups with:

```text
CURLY_CONFIG_DIR
CURLY_DATA_DIR
```

The layout is:

```text
<config root>/
  config.toml
  requests/
    <name>.toml

<data root>/
  history.sqlite3
```

## Exit codes

| Code | Meaning |
| ---: | --- |
| 0 | Success |
| 1 | Transport or local I/O failure |
| 2 | Invalid arguments or configuration |
| 22 | HTTP 4xx/5xx with `--fail` |
| 130 | Cancellation |

History-write failures emit a warning without replacing an otherwise successful HTTP result.

## Releases

The release workflow currently builds and packages only `x86_64-pc-windows-msvc`. Linux and macOS release targets are intentionally deferred until they can be built, smoke-tested, and inspected on those platforms.

The Windows archive gets a SHA-256 checksum and a runtime dependency report. Public release publication is intentionally blocked until the owner adds a chosen `LICENSE`, `LICENSE.txt`, or `LICENSE.md` file.

## Project documents

- [Agent instructions](AGENTS.md)
- [Original specification](docs/SPEC.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Implementation plan](docs/PLAN.md)
- [Testing and release checks](docs/TESTING.md)
- [Contributing](CONTRIBUTING.md)
- [Security and local data](SECURITY.md)

Multipart/forms, cookie jars, explicit proxy controls, collections/environments, curl import/export, plugins, and advanced TUI editing beyond the current fast request composer remain outside v1 scope.
