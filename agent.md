# Agent entry point

The project instructions live in [AGENTS.md](AGENTS.md), the conventional filename used by coding agents. Read that file and its linked specification before working on cURLy.

## Current TUI product direction

`curly tui` is an interactive request workspace, not only a history browser. It should make common API requests fast to compose and send from the keyboard, show the response immediately, retain searchable history/replay, and let users load replayable history entries back into the request composer for editing and resending. This current direction supersedes older history-only or "no TUI request editing" wording in the preserved original specification.

## Standing Windows install requirement

After any change that affects the executable, once the Windows release build and required checks succeed, refresh the user's command-line installation automatically. Copy the verified release binary to `%USERPROFILE%\.local\bin\curly.exe`; that directory is already on the user's PATH. Do not wait for the user to ask for PATH installation again.

Before finishing executable-changing work, verify from a fresh shell that `where curly` resolves to `%USERPROFILE%\.local\bin\curly.exe` and that `curly --version` runs successfully. Keep the installed binary synchronized with the latest verified release build.
