# ADR-0003 — In-Process Engine on Tokio & Custom Segmented HTTP Engine

- Status: Accepted (2026-11-12)

## Context
Ruscello needs a fast (multi-connection, dynamic work-stealing), crash-safe HTTP download engine controllable from the
GUI without blocking the UI thread. Running a separate background daemon adds lifecycle and IPC complexity not needed
for the desktop app.

## Decision
- `ruscello-engine` is a **library crate** running on a multi-threaded `tokio` runtime within the same process as the
  GUI (or CLI). Its sole boundary is `EngineHandle` (`mpsc` commands, aggregated `broadcast` events).
- The HTTP download engine (`ruscello-http`) is **written in-house** on top of `reqwest`/`hyper`, featuring an
  aria2/IDM-style segment allocator with dynamic work stealing and custom crash-safe persistence (ADR-0006). Existing
  crates like `gosh-dl` serve as reference material, but owning `ruscello-http` gives us full control over resume
  invariants, expired-link re-resolution, and per-hoster connection policies.

## Alternatives Considered
- Separate daemon process with RPC (like `qbittorrent-nox` / `aria2c --enable-rpc`): can still be added later by
  wrapping `ruscello-engine` + `ruscello-api` in a headless binary without changing any core crate.
- Using `aria2c` as an external binary: not pure Rust, harder to integrate tightly with hoster re-resolution.

## Consequences
- If the GUI process crashes, active downloads stop until relaunch; mitigated by crash-safe persistence (automatic
  resume on startup) and task-level panic isolation.
