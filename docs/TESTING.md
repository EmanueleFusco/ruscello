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
| BitTorrent | `crates/torrent/tests/` | local seeder + tracker | Multi-file torrent, interruption, resume, stale checkpoints and piece hash recheck |
| Fuzzing | `fuzz/` | `cargo-fuzz` | Parsers exposed to untrusted remote input (release check) |
| Benchmarks | `benches/`, `scripts/bench*` | `criterion`, comparison vs `aria2` | Resource & throughput targets in `PLAN.md` §2 |
| Live Network | gated by `live-tests` feature | real internet | Real hoster/site checks; manual or non-blocking scheduled workflow |

The vendored UDP receive regression runs separately with
`cargo test --manifest-path vendor/librqbit-dualstack-sockets/Cargo.toml --lib receive::tests`.
It injects address and routing errors and checks retry timing, recovery and cancellation without a live
network. The macOS run also checks the Darwin error mapping. Native sleep, wake and Wi-Fi reconnection
remain separate acceptance tests with a public magnet and a controlled torrent.

The vendored date changes have focused checks for persisted DHT timestamps, HLS playlist round trips,
and FTP timestamp parsing. Run them alongside the normal FTP resume and media integration tests.

```text
cargo test --manifest-path vendor/librqbit-dht/Cargo.toml --lib peer_store::tests
cargo test --manifest-path vendor/m3u8-rs/Cargo.toml --test lib
cargo test --manifest-path vendor/suppaftp/Cargo.toml --lib list::tests
cargo test --manifest-path vendor/suppaftp/Cargo.toml --lib datetime::tests
```

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

For macOS associations, install the built application bundle in a disposable account. Use Finder's
Open With menu for a local `.torrent` and `.dlc`, including names with spaces and Unicode. Open a
magnet from a browser. Repeat each action with Ruscello closed and while its window is running.
Confirm that the existing Add flow receives each input and that Finder's current default stays selected.

The Download Settings workflow checks both supported languages, applying presets through Save,
manual limits, and restart persistence. The HTTP transport tests check that an exact-host override
leaves the other hosts' limits in effect and that lowering a cap lets in-flight requests drain.
The HTTP integrity tests check that a per-file override follows the effective source after redirects,
respects a tighter resolver cap, and gives unrelated mirrors the ordinary budget.

The BitTorrent transport regression uses a TCP listener that accepts a socket without completing the
peer handshake and a UDP relay that drops the first two µTP SYNs. It checks that retransmissions
keep the connection ID and sequence number, then exercises pause and resume under a one-peer budget.
| M9 FTP/FTPS | `cargo test -p ruscello-http --test ftp` | Loopback transfers, TLS, interrupted transfers, `REST` fallback, and credential redaction |

## Mandatory CI Gates (Every PR)

1. `cargo fmt --all --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace` (via `cargo-nextest`) on `windows-latest`, `ubuntu-latest`, `macos-latest`
4. `cargo deny check`
5. Native browser tests on Windows, Linux (under Xvfb), and macOS.

For `v*` tags, Desktop packages runs Hardening on the tagged commit and waits for it to pass before creating
the GitHub release draft. Hardening runs parser fuzzing, a short soak, resource and throughput benchmarks,
and crash/recovery tests. Maintainers can also run Hardening manually from the Actions tab.

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
| Timed checkpoint budget | `cargo test -p ruscello-cli --release --test crash_resume crash_recovery_refetches_at_most_two_seconds_of_data -- --ignored --nocapture` |
| Dependency and path audits | `cargo deny check` and `./scripts/check-personal-paths.sh` |
| Package builds | Run the **Desktop packages** GitHub workflow or follow the platform commands in `packaging/README.md`. |

The soak creates successive batches of 200 transfers with seeded ranges, single-stream downloads, redirects,
varying latency, and dropped connections. It restarts the same queue after forced process exits and compares
every final file against the original generated bytes using SHA-256. Linux reports process RSS and enforces
a bound relative to the first workers. Tagged releases use a 90-second soak. The optional full soak needs
a Linux runner labeled `ruscello-soak-linux` or a local machine.

The 1.0 validation used an owner-approved abbreviated soak. An earlier run stopped after 39 minutes,
11,400 verified files, and 205 forced exits when an expected output filename was missing. A regression
test reproduced duplicate publication when saving the completed state failed. The scheduler now retains
publication state until the completion is committed. A release build then passed six batches in 242 seconds
with 22 forced exits, 1,200 SHA-256 comparisons, and worker RSS within the harness bounds.

The desktop benchmark reports process RSS in decimal MB, idle CPU over three seconds, and time from process
launch until the window and local queue API are ready. It counts consecutive progress changes per download to
sample the scheduler's four-per-second reporting cadence, with a small allowance for sampling at interval
boundaries. It normalizes user-mode CPU at the measured throughput to 1 Gbit/s and requires loopback
throughput to reach that rate. Run with an actual display and GPU to measure the default renderer.
Hardening records an initial warm-up launch on the hosted runner. It then starts a new process with a fresh
profile for the measured run, so the one-second limit applies after shared host caches have been initialized.
For CPU profiling on Linux, run `perf record -g -- ./scripts/bench-resources.py` in the same environment
and inspect the call stacks with `perf report`. Results depend on the filesystem, CPU, display and renderer.

### Linux release validation

The following measurements used the default release profile on an AMD Lucienne desktop with X11.
The software run used Xvfb.

| Budget | Software 2D | Hardware GPU |
| --- | ---: | ---: |
| Idle RAM | 46.4 MB | 127.0 MB |
| RAM with 10 downloads | 66.3 MB | 139.1 MB |
| Idle CPU | 0.0% | 0.0% |
| User CPU normalized to 1 Gbit/s | 1.4% | 1.4% |
| Cold start to usable window | 0.112 s | 0.174 s |
| Per-row progress updates | 2.96 Hz | 2.96 Hz |

The 1.0 release has an owner-approved exception to the 80 MB idle RAM target for the default Linux GPU
renderer on this machine. The target remains open for future optimization. A minimal Iced window using
the same default GPU backend used 93.9 MB RSS before loading any Ruscello components. The packaged build
also exceeded the target with alternate GPU settings. Vulkan used 110.9 MB on X11 and 111.6 MB on
Wayland. Restricting it to the AMD RADV driver reduced those results to 105.8 MB and 107.6 MB,
respectively. OpenGL used 135.4 MB on Wayland. The default renderer and driver selection are unchanged.

The process-kill benchmark
observed a 2.001-second checkpoint gap and an upper bound of 3,538,944 bytes to refetch
at a configured 2 MiB/s. The resumed file matched its fixture's SHA-256.
With 64 KiB write batches, the segmented throughput repeat measured 596.7 MiB/s
against aria2's 540.3 MiB/s (110.43%). The timed checkpoint test and all three
ordinary crash-resume tests also passed with the smaller batches. The local kernel
restricted access to `perf` counters.

### Windows release-mode follow-up

On the Windows QA host, three runs per renderer used fresh profiles and an optimized source
build with a test-only executable name and settings directory. Each run transferred ten 2 MiB
files over loopback with a 2 MiB/s global cap, and all 60 files matched the expected SHA-256.

| Working set (decimal MB) | Software 2D | Hardware GPU (DX12) |
| --- | ---: | ---: |
| Idle GUI process, median of the three per-run maxima | 38.2 | 133.2 |
| Ten active downloads, peak GUI process | 46.3 | 142.9 |
| Ten active downloads, peak sum of GUI and launcher | 57.4 | 154.0 |
| Minimal Iced window, median of three settled samples | 23.6 | 103.8 |

The sum of process working sets can count shared pages twice. A separate single Vulkan run
measured 205.2 MB peak idle and 169.8 MB with ten active downloads in the GUI process. On this host,
the idle GPU target is exceeded by the minimal Iced window as well. A renderer policy change
needs separate graphics and high-DPI acceptance. The Windows GPU memory budget remains open.

The normal close event hid the window in 10–13 ms. From a seeding session the process exited in
1.19 s and reopened with its saved row and unchanged contents. After removing the torrent row, it
exited in 1.16 s and reopened with the row still absent.

One confirmation run with the fair shared limiter measured 132.8 MB peak idle and 143.3 MB under
load in the DX12 GUI, and 38.0 MB idle and 46.9 MB under load in the Software 2D GUI.

The Windows desktop now selects the saved renderer in the original process. On the GeForce RTX 3080
test host (driver 32.0.16.2101), the previous build reached 154.6 MB summed working set with ten active
downloads. Starting that same build with its renderer environment configured in advance reached
142.7 MB with one process. The launcher previously used up to 2.3 MB of private commit. Summed working
sets may count shared pages twice.

| Single-process release build on Windows | Hardware GPU (DX12) | Software 2D |
| --- | ---: | ---: |
| Idle working set, median of three per-run maxima | 132.9 MB | 38.1 MB |
| Peak working set with ten active downloads | 143.0 MB | 47.4 MB |

Each of the six runs verified ten 2 MiB payloads with SHA-256. Both torrent shutdown scenarios
reopened with the expected queue state and verified payload. The minimal Iced DX12 window used
103.7–103.9 MB under the low-power adapter preference on this host. The 80 MB idle budget remains
open. Hardware GPU remains the default and explicit backend overrides are preserved.
