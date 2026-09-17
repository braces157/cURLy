# Contributing

Read [AGENTS.md](AGENTS.md), the [specification](docs/SPEC.md), and the [implementation plan](docs/PLAN.md) before changing behavior. Preserve the shared request model/executor and the exact stdout/stderr contract.

Use stable Rust with the committed lockfile. Before submitting changes, run:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
```

Tests must use local servers, temporary storage directories, and synthetic credentials. Do not depend on public APIs or commit real tokens, personal history databases, or private request bodies. Add tests for changed observable behavior and meaningful failure modes rather than mirroring implementation details.

Release changes should preserve the verified Windows x64 artifact, checksum generation, and runtime dependency inspection. Linux and macOS support should only be added when their builds and smoke checks can actually be verified.

The repository does not yet specify a license. Licensing remains an owner decision; the release workflow blocks public publication until a license file is present.
