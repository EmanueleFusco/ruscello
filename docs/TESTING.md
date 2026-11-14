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
Loopback pages cover hoster links embedded in scripts, XFileSharing-like forms and countdowns, and
verification challenges. The testkit also includes an FTP server with explicit FTPS and fault injection.

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

## Native browser verification tests

From a desktop session, run `cargo test -p ruscello-webview --test browser -- --ignored --test-threads=1`.
Linux CI runs the same command under Xvfb. These tests use loopback pages to simulate the user's verification
click, check that five queued downloads complete using the HttpOnly cookie and browser User-Agent, and reopen
the helper to check profile persistence. They also verify native popup blocking, download handoff to the
segmented HTTP engine, and LinkGrabber capture of fetch, XHR, and media URLs. The XFileSharing batch asserts
that all ten files complete, no transfer starts before the countdown, and no popup reaches the server.
`cargo test -p ruscello-browser` checks cookie imports spanning multiple requests, idle shutdown, and
replacement of a crashed helper using a local protocol fixture. The headless iced workflow checks the
verification and import controls in English and Italian.

## Media, remote control, and automation tests

| Milestone | Command | Coverage |
|-----------|---------|----------|
| M7 Media | `cargo test -p ruscello-media --test media_streams` | HLS AES-128, segment resume, DASH, and sidecar integration using local fixtures |
| M8 LAN and browser extension | `cargo test -p ruscello-api` | Loopback authentication, LAN sessions, extension pairing, and live queue events |
| M9 Automation | `cargo test -p ruscello-engine --test automation` | Schedule, queue completion and sleep inhibition with mock power controls, checksums, extraction, hooks, watch folders, and FTPS resume |
| M9 FTP/FTPS | `cargo test -p ruscello-http --test ftp` | Loopback transfers, TLS, interrupted transfers, `REST` fallback, and credential redaction |

## Mandatory CI Gates (Every PR)

1. `cargo fmt --all --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace` (via `cargo-nextest`) on `windows-latest`, `ubuntu-latest`, `macos-latest`
4. `cargo deny check`
5. Native browser tests on Windows, Linux (under Xvfb), and macOS.

The Hardening workflow runs a nightly fuzz campaign, a reduced soak, and benchmark threshold checks.

GitHub Actions runs the JavaScript checkout and Rust cache actions on Node.js 24. Local builds and tests
use the Rust toolchain.

## Testing Rules

- Deterministic tests: avoid arbitrary `sleep` calls; use injectable clocks or event notifications with timeouts.
- Every bug fix must include a regression test.
- Tests must never write outside `tempfile::TempDir`.

## M10 release checks

| Check | Command |
|---|---|
| Updater metadata, download integrity and replacement | `cargo test -p ruscello-gui updater` |
| Headless updater and checkpoint workflow | `ICED_TEST_BACKEND=tiny-skia cargo test -p ruscello-gui update_banner` |
| Sanitized parser campaign | Install nightly Rust and `cargo-fuzz`, then run `./scripts/run-fuzz.sh 60` |
| 24-hour loopback soak | `cargo test --release --locked -p ruscello-cli --test soak mixed_download_soak -- --ignored --nocapture` |
| Short soak | Prefix that command with `RUSCELLO_SOAK_SECONDS=90` |
| Linux desktop resource budget | Build `ruscello-gui` and `ruscello-cli` with `--release --locked`, then run `./scripts/bench-resources.py` in an empty X11 session. For software rendering, use `xvfb-run -a ./scripts/bench-resources.py --backend Software`. |
| Throughput and crash resume | Use the aria2 benchmark above and run `cargo test -p ruscello-cli --release --test crash_resume`. |
| Dependency and path audits | `cargo deny check` and `./scripts/check-personal-paths.sh` |
| Package builds | Run the **Desktop packages** GitHub workflow or follow the platform commands in `packaging/README.md`. |

The soak creates successive batches of 200 transfers with seeded ranges, single-stream downloads, redirects,
varying latency, and dropped connections. It restarts the same queue after forced process exits and compares
every final file against the original generated bytes using SHA-256. Linux reports process RSS and enforces
a bound relative to the first workers. The nightly workflow uses a short soak. The optional full soak needs
a Linux runner labeled `ruscello-soak-linux` or a local machine.

The desktop benchmark reports process RSS in decimal MB, idle CPU over three seconds, and time from process
launch until the window and local queue API are ready. It counts consecutive progress changes per download to
sample the scheduler's four-per-second reporting cadence, with a small allowance for sampling at interval
boundaries. It normalizes user-mode CPU at the measured throughput to 1 Gbit/s and requires loopback
throughput to reach that rate. Run with an actual display and GPU to measure the default renderer.
For CPU profiling on Linux, run `perf record -g -- ./scripts/bench-resources.py` in the same environment
and inspect the call stacks with `perf report`. Results depend on the filesystem, CPU, display and renderer.
