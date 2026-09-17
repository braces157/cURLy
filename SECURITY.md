# Security and local data

TLS certificate verification is enabled by default. `--insecure` is the explicit opt-out for local development. Redirects require `--follow` and are capped at ten hops. The reqwest client is configured with a never-retry policy.

History is local and defaults on only when stdout is a terminal. Retention is limited to the newest 1,000 entries and request/response previews are capped at 64 KiB each. Authorization, proxy authorization, cookie, and set-cookie values are redacted case-insensitively before persistence.

Literal basic and bearer authentication values are not persisted in replay metadata. Saved requests reject literal authentication credentials and credential-bearing headers; environment-variable references are preserved without storing their resolved values. Replay rejects missing credentials, missing files, stdin-backed bodies, truncated/incomplete bodies, and other unavailable inputs before sending a request.

**URLs, query parameters, and bodies may still contain sensitive data.** Header redaction does not make a history database safe to share. Local configuration and SQLite history are not encrypted by `curly`.

Terminal display escapes control characters. Raw pipeline and download bytes are never sanitized or decorated because changing them would violate the byte-preserving output contract.

Do not include real credentials or sensitive history data in public bug reports. This repository does not currently designate a private vulnerability-reporting contact; the owner should establish one before public distribution.
