Build a Rust HTTP client focused on JSON APIs, with URL-first commands, reliable pipelines, saved requests, and an optional TUI for browsing and replaying history.
Start from the empty workspace. Deliver one executable per supported target, with no server, account, or separately installed application libraries.
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
- Support -X/--method, repeatable -H/--header and --query, mutually exclusive --json/--body, basic authentication, and bearer authentication through a literal value or environment-variable reference.
- Default to GET; supplying a body defaults to POST unless a method is explicit. Require an HTTP/HTTPS URL. Read stdin only when explicitly requested with -; @path reads a body file.
- Provide --output, --raw, --headers, --quiet, --color auto|always|never, --fail, and --history/--no-history.
- Detect stdout using Rust’s [IsTerminal](https://doc.rust-lang.org/stable/std/io/trait.IsTerminal.html). Pipes and files receive unformatted response-body bytes without added newlines; diagnostics and requested headers go to stderr.
- Terminal output formats and highlights valid JSON up to 1 MiB and prints status/timing to stderr. Larger responses stream without formatting. Escape terminal control characters in displayed text; show a summary for binary content.
- Set a 10-second connection timeout and 30-second total timeout, configurable with flags; allow disabling the total timeout. Follow redirects only with --follow, capped at ten hops. Do not automatically retry requests.
- Verify TLS certificates by default. Provide explicit --insecure for local development.
- Define exit codes: 0 success, 1 transport/local I/O failure, 2 invalid arguments/configuration, 22 HTTP 4xx/5xx with --fail, and 130 cancellation. Preserve response bodies with --fail; handle downstream broken pipes quietly.
- Use one Cargo package with a thin binary and internal modules for request modeling, execution, output, storage, and TUI. Both interfaces share the same request executor.
- Use Tokio, reqwest with Rustls, clap, serde/serde_json, TOML, Ratatui/Crossterm, and rusqlite with [bundled SQLite](https://github.com/rusqlite/rusqlite). Commit the dependency lockfile.
- Represent requests independently of clap, including method, URL, ordered headers/query parameters, body source, authentication, and transport options. Stream response chunks to output while retaining only bounded history previews.
- Store configuration and named requests in platform-standard configuration directories; store history in the platform-standard local data directory. Support CURLY_CONFIG_DIR and CURLY_DATA_DIR overrides.
- Save one versioned TOML file per named request. --save NAME saves the definition and executes it; reject existing names unless --overwrite is supplied. Provide saved-request list/show/delete commands.
- Preserve environment-variable references without storing resolved credentials. Resolve saved body-file paths relative to the request file. Reject saving stdin-backed requests with guidance to use a file.
- Record history by default when stdout is a terminal; scripts opt in. Retain the newest 1,000 entries, with request and response body previews capped at 64 KiB each and explicit truncation markers.
- Store timestamps, request details, status/errors, elapsed time, headers, byte counts, and previews. Redact authorization, proxy authorization, cookie, and set-cookie values before persistence; document that bodies and URLs can still contain sensitive data.
- Replay only complete requests with available bodies and credentials. Preserve environment references for replay; otherwise report missing inputs rather than sending redaction placeholders.
- Use versioned SQLite migrations, transactions, and a bounded busy timeout. History-write failures warn without changing a successful HTTP exit code. Provide history clearing; keep persistence work off the TUI event loop.
- Launch only through curly tui; require interactive stdin and stdout. Running without arguments prints CLI help.
- Show a searchable history list beside request/response details, with header and body views, JSON highlighting, scrolling, and visible truncation indicators.
- Support arrows or j/k, Tab to change focus, / to filter, r to replay, and q to quit. Confirm replay of methods other than GET, HEAD, and OPTIONS.
- Execute requests asynchronously with a loading indicator and cancellation. Record TUI replays in history.
- Restore terminal state on normal exit, errors, and panic. Exclude request editing from v1.
- Test CLI parsing, body sources, duplicate headers/query parameters, authentication, redirects, timeouts, TLS failure, HTTP errors, and cancellation against local test servers.
- Verify exact piped bytes, binary downloads, bounded-memory streaming, stderr separation, broken pipes, terminal formatting, and --fail exit codes.
- Test TOML round trips, environment-reference handling, redaction, truncation, retention, migrations, concurrent history writes, and replay rejection when inputs are missing.
- Test TUI state transitions with Ratatui’s test backend; manually verify resizing, replay, cancellation, and terminal restoration on Windows, macOS, and Linux.
- Run formatting, Clippy, and tests in CI. Produce Windows x64, Linux x64-musl, and macOS Intel/Apple Silicon release artifacts with checksums. Use static linking where supported and inspect resulting dependencies; Rust’s [runtime-linking support is target-dependent](https://doc.rust-lang.org/reference/linkage.html).
- Implement in order: CLI/executor → output contract → persistence/replay → TUI → release packaging and documentation.
- Defaults: executable name curly; personal JSON-API workflow; JSON-only syntax highlighting initially. Defer multipart/forms, cookie jars, explicit proxy controls, collections/environments, curl import/export, plugins, and a full TUI editor.