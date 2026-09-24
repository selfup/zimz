# Claude Code Instructions

## Go

- **Never use `go build`** — do not compile Go code into binaries under any circumstances
- Always run Go programs with `go run <path>`
- Use `go vet <path>` for static analysis
- Use `go fix <path>` for automated fixes
- Use `go test <path>` for tests

This keeps the repo free of committed binaries and ensures all Go is run from source.

## Rust

- Use `cargo fmt`
- Use `cargo check` before `cargo run`
- No Warnings

## Git

- **Never run `git push`, `git pull`, or `git fetch`** — the user controls when code moves between local and remote
- **Never run destructive git commands** (`push --force`, `reset --hard`, `branch -D`, `clean -f`, `rebase`, etc.) under any circumstances
- Only create commits when the user explicitly asks (`commit`, `commit and push`, etc.)
- For any other git operation that mutates history or shared state, ask first

## Pyhton

Use uv to install deps, and run python code. uv only
