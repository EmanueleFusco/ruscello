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
// illustrative API
let srv = TestServer::start().await;
let url = srv.file(FileSpec::random(seed, 256 * MIB)
    .ranges(true)
    .etag("v1")
    .drop_after_bytes(10 * MIB)        // simulates dropped connection
    .throttle_per_conn(2 * MIB)        // bytes/sec per connection
    .max_conns_per_ip(4)               // returns 503 above limit
    .latency(Duration::from_millis(50)));
```

Also serves mock HTML pages replicating real hoster patterns (fuckingfast-like: `/dl/...` direct link embedded in
script; XFileSharing-like: `method_free` form → countdown → `downloadbtn`; mock verification challenge page) so
plugins, TOML recipes, and the browser helper are tested **completely offline**.

## Fixtures

- Binary payloads are generated deterministically from seeds at test time (never committed to git).
- Sanitized HTML fixtures are stored under `crates/resolver/tests/fixtures/` with capture date in the filename.

## Mandatory CI Gates (Every PR)

1. `cargo fmt --all --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace` (via `cargo-nextest`) on `windows-latest`, `ubuntu-latest`, `macos-latest`
4. `cargo deny check`
5. (From M10) Nightly job: short fuzzing campaign, reduced soak test, benchmark threshold check.

## Testing Rules

- Deterministic tests: avoid arbitrary `sleep` calls; use injectable clocks or event notifications with timeouts.
- Every bug fix must include a regression test.
- Tests must never write outside `tempfile::TempDir`.
