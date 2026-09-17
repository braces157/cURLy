# cURLy

`curly` is a single-binary Rust HTTP client for personal JSON API workflows. The CLI is the primary interface; `curly tui` opens an optional history browser and replay UI.

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

`curly tui` requires interactive stdin and stdout. It shows searchable history beside request/response details and uses the same executor/replay validation as the CLI.

Controls:

- `↑`/`↓` or `j`/`k`: move through history or scroll the focused details pane.
- `Tab`: switch focus between history and details.
- `h`: show headers.
- `b`: show request/response body previews.
- `PageUp`/`PageDown` and `Home`: scroll details.
- `/`: filter history.
- `r`: replay; methods other than GET, HEAD, and OPTIONS require confirmation.
- `Esc`: cancel an active replay.
- `q`: quit.

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

Multipart/forms, cookie jars, explicit proxy controls, collections/environments, curl import/export, plugins, and TUI request editing remain outside v1 scope.
