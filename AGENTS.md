# Repository Guidelines

## Project Structure & Module Organization

The Rust workspace lives under `crates/`. Core language and browser/runtime code is in `crates/boon`, and the CLI is in `crates/boon-cli`. Shared contracts for the current architecture split live in `crates/boon-scene`, `crates/boon-monitor-protocol`, and `crates/boon-renderer-zoon`.

The interactive app is in `playground/`: `frontend/` contains examples and the web UI, `backend/` hosts the Moon app backend, and `shared/` holds cross-app types. Automation and browser-test tooling live in `tools/`, especially `tools/src/`, `tools/scripts/`, and `tools/extension/`. Design notes and plans are in `docs/`.

## Build, Test, and Development Commands

- `cargo check -p boon`: fast validation for the main library crate.
- `cargo test -p boon`: run Rust tests for the main crate.
- `cd playground && makers mzoon start`: run the local playground at the configured workspace port, currently `http://localhost:8086`.
- `cd tools && cargo run --release -- server start --watch ./extension`: start the browser automation server.
- `./tools/scripts/verify_7guis_complete.sh --static-only`: run static 7GUIs verification.
- `cd tools && just test-examples --filter cells --engine Actors`: run a targeted browser-driven example test.

### Playground Dev Stack

For normal playground development, prefer a single long-running `mzoon` process:

- use `cd playground && makers mzoon start`
- do not start separate duplicate frontend/backend runners when `mzoon` is sufficient
- treat `mzoon` output as the primary place to watch frontend build results, backend build results, rebuilds, and running-instance logs

### Browser Automation Triage

When the browser extension is not working, debug in this order first:

- verify the browser is actually started with the shared `tools/.chrome-profile`
- verify the WebSocket server is running for extension communication: `cd tools && cargo run --release -- server start --watch ./extension`
- verify the current playground example has not frozen or wedged the browser tab
- verify the shared Chrome/Chromium profile was not reset and Developer mode is still enabled in `chrome://extensions/`

Only start debugging the extension code itself after those checks are verified. Most extension failures in this repo come from browser/session state, missing WebSocket server, a wedged example, or Developer mode being turned off after the shared profile was disturbed.

Do not reset, scrub, or auto-recreate the shared Chrome profile to "fix" extension issues.

## Coding Style & Naming Conventions

Use standard Rust formatting with `cargo fmt`. Follow existing Rust naming: `snake_case` for functions and modules, `CamelCase` for types, and descriptive names over abbreviations. Keep files ASCII unless the file already uses Unicode. Prefer small, explicit changes over broad rewrites.

## Testing Guidelines

Favor targeted checks before broad suites. Example behavior is validated through `.expected` files and browser automation in `tools/scripts/`. When changing playground behavior, update or add the matching example assets under `playground/frontend/src/examples/`. Keep tests deterministic and cross-engine when possible.

For browser-driven failures, check browser started, WebSocket server running, current example health, and shared-profile Developer mode before assuming `tools/extension/` code is broken.

## Commit & Pull Request Guidelines

This repo uses `jj`; prefer `jj status`, `jj diff`, and `jj log` over `git` commands. Recent history favors descriptive, sentence-case commit subjects, often followed by short bullet lists for grouped changes. Keep commits scoped to one logical change.

Pull requests should explain user-visible impact, affected engines or renderers, verification performed, and any remaining gaps. Include screenshots for playground or renderer changes.

## Architecture Notes

Current milestone order is: Zoon parity across all engines first, persistence second, canvas renderer third, RayBox later. Shared crates should define contracts, not a shared runtime core.
