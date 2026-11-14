# Testing Strategy

"Stable and tested" is a core requirement: no feature is considered complete without automated tests.

## Test Levels

| Level | Location | Tools | Notes |
|-------|----------|-------|-------|
| Unit | inside each crate (`#[cfg(test)]`) | `cargo test`, `proptest` | Segment allocator, state machine, parsers (`Content-Disposition`, `m3u8`, TOML recipes), LinkGrabber scoring |
| Property-based | `ruscello-http`, `ruscello-core` | `proptest` | Invariants: byte range coverage, zero overlap, monotonic progress, valid state transitions |
| Integration | `crates/*/tests/` | `ruscello-testkit`, `tempfile`, `tokio::test` | End-to-end downloads against the local fault-injecting HTTP test server |
| Crash / Resume | `apps/cli/tests/` | child process `ruscello-cli` + kill | Forcefully kill process at randomized points (reproducible seed) → restart → verify SHA-256. **Mandatory in CI** |
| Snapshot | `resolver`, `gui` | `insta` | LinkGrabber output on HTML fixtures; `iced` UI views |
| GUI | `apps/gui/tests/` | `iced_test` | User flows: add, pause, resume, cancel, sidebar filtering, language switching (`en`/`it`) |
| BitTorrent | `crates/torrent/tests/` | local seeder + tracker | Multi-file torrent, interruption, resume, piece hash recheck |
| Fuzzing | `fuzz/` | `cargo-fuzz` | Parsers exposed to untrusted remote input (nightly job) |
| Benchmarks | `benches/`, `scripts/bench*` | `criterion`, comparison vs `aria2` | Resource & throughput targets in `PLAN.md` §2 |
| Live Network | gated by `live-tests` feature | real internet | Real hoster/site checks; manual or non-blocking scheduled workflow |

## Testkit (`ruscello-testkit`)

In-process local HTTP server bound to an ephemeral port with a scenario builder:

```rust
let srv = TestServer::start().await?;
let file = srv.file(FileSpec::random(seed, 256 * MIB)
    .ranges(true)
    .drop_after_bytes(10 * MIB)        // simulates dropped connection
    .throttle_per_conn(2 * MIB)        // bytes/sec per connection
    .max_conns_per_ip(4, StatusCode::SERVICE_UNAVAILABLE)
    .latency(Duration::from_millis(50)));
let url = file.url();
```

The fixture handle also exposes the seeded payload and records range requests and response statuses.
Later milestones add mock HTML pages replicating real hoster patterns (fuckingfast-like: `/dl/...` direct link embedded in
script; XFileSharing-like: `method_free` form → countdown → `downloadbtn`; mock verification challenge page) so
plugins, TOML recipes, and the browser helper are tested **completely offline**.

## HTTP throughput benchmark

Install `aria2c` or set `ARIA2C` to its executable path, then run:

```sh
cargo test -p ruscello-cli --release --test http_benchmark -- --ignored --nocapture
```

The benchmark compares five alternating runs of each client using 16 connections, a seeded 256 MiB file,
and 50 ms of loopback response latency per request. It checks each result against the expected SHA-256
outside the timed interval and requires Ruscello's median throughput to reach at least 95% of aria2's.

## Fixtures

- Binary payloads are generated deterministically from seeds at test time (never committed to git).
- Sanitized HTML fixtures are stored under `crates/resolver/tests/fixtures/` with capture date in the filename.
- The offline LinkGrabber ranking benchmark uses 30 saved synthetic HTML pages with expected targets.
  Run `cargo test -p ruscello-resolver --test scoring -- --nocapture` to report elapsed time and top-three accuracy.
- `cargo test -p ruscello-engine --test resolver` checks the 20-page engine batch, package persistence, and
  refreshed links and cookies. `cargo test -p ruscello-cli --test crash_resume` covers a batch with a process
  kill, URL renewal, a dropped network response, and SHA-256 comparisons. Both use loopback fixtures.

## Mandatory CI Gates (Every PR)

1. `cargo fmt --all --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace` (via `cargo-nextest`) on `windows-latest`, `ubuntu-latest`, `macos-latest`
4. `cargo deny check`
5. (From M10) Nightly job: short fuzzing campaign, reduced soak test, benchmark threshold check.

GitHub Actions runs the JavaScript checkout and Rust cache actions on Node.js 24. Local builds and tests
use the Rust toolchain.

## Testing Rules

- Deterministic tests: avoid arbitrary `sleep` calls; use injectable clocks or event notifications with timeouts.
- Every bug fix must include a regression test.
- Tests must never write outside `tempfile::TempDir`.
