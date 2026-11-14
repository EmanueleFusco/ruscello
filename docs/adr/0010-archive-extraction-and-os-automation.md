# ADR-0010: Archive Extraction, FTP/FTPS, and OS Automation

## Status

Accepted

## Context

Milestone M9 adds desktop automation and post-processing workflows to Ruscello:

1. Automatic archive extraction for `.zip`, `.7z`, `.tar.*`, and `.rar` (including multi-part archives and password lists).
2. Automatic checksum verification (`.sfv`, `.md5`, `.sha256`), watch folders (`.torrent`, `.dlc`, `.txt`), time-of-day speed/queue scheduling, post-queue completion actions (notify, sleep, shut down), and a post-download command hook.
3. OS sleep inhibition during active downloads across Windows, Linux, and macOS.
4. FTP and FTPS transfers with `REST` resume (deferred from M1.11).

Three workspace constraints from earlier ADRs apply directly:

- **GPL-3.0-or-later license compatibility (ADR-0007):** All linked crates must be GPL-compatible and pass `cargo deny check`, and `openssl-sys` is disallowed in favor of `rustls`. Proprietary or UnRAR-restricted decoders cannot be linked statically or dynamically into Ruscello binaries.
- **Sidecar isolation (ADR-0002):** External tools that cannot be bundled under GPL-3.0-or-later may only be invoked as optional external subprocesses discovered on `PATH` or configured explicitly by the user.
- **Testable platform abstractions (ADR-0003, ADR-0007):** OS sleep inhibition, OS power transitions, and wall-clock scheduling must sit behind traits and `#[cfg]` platform modules so automated tests remain deterministic and never suspend or shut down test machines.

## Decision

1. **Pure-Rust extraction + external `.rar` subprocess:**
   - Extract `.zip`, `.7z`, `.tar`, `.tar.gz`/`.tgz`, `.tar.xz`/`.txz`, and `.tar.bz2`/`.tbz2` entirely in-process using permissive-licensed pure-Rust crates (`zip`, `sevenz-rust2`, `tar`, `flate2`, `lzma-rs`, `bzip2-rs`), enforcing strict archive path canonicalization (Zip-Slip protection) and optional password iteration.
   - Support `.rar` (including `.partN.rar` and `.rNN` multi-part volumes) exclusively via an external `7z`, `7zz`, or `unrar` CLI subprocess with a configured path or `PATH` lookup. If neither binary is available when a `.rar` archive finishes, Ruscello skips `.rar` extraction cleanly with an actionable log/status message rather than crashing or linking non-free code.
   - Delay extraction of multi-part volumes until all downloads belonging to the same `Package` (or sibling parts) have finished transferring.

2. **FTP/FTPS on top of `suppaftp` + `rustls`:**
   - Implement FTP and FTPS (`AUTH TLS` explicit TLS and implicit FTPS) via `suppaftp` configured with `tokio-rustls-aws-lc-rs`, matching the workspace's existing `rustls` stack and avoiding OpenSSL.
   - Reuse `ruscello-http`'s crash-safe positional `FileWriter` and `.rscpart.ctl` control file (`SIZE` + `REST <offset>`) so interrupted FTP/FTPS downloads preserve the *fsync data first → update control file second* invariant.
   - Strip embedded FTP passwords before persisting download metadata or `.rscpart.ctl` checkpoints or writing `tracing` logs; keep session credentials in transient engine memory or the OS keyring.

3. **Platform isolation in `ruscello-platform`:**
   - Implement OS sleep inhibition in `crates/platform` behind a shared trait:
     - **Windows:** Dedicated worker thread calling `SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED)` and clearing with `ES_CONTINUOUS` on the same thread (honoring Win32 per-thread state affinity).
     - **Linux:** `org.freedesktop.login1.Manager.Inhibit("sleep", ...)` over D-Bus via `zbus`, holding the returned `OwnedFd` while transfers or post-processing are active and degrading gracefully when `logind` is unavailable.
     - **macOS:** `IOPMAssertionCreateWithName` (`PreventUserIdleSystemSleep`) and `IOPMAssertionRelease` via `IOKit` + `CoreFoundation` frameworks.
   - Expose cancellable queue-completion power actions (notify, sleep, shut down) behind an injectable `PowerController` trait so engine and headless GUI tests verify countdowns, cancellation, and invocation without calling OS power endpoints.

## Alternatives Considered

- **Linking `unrar` C++ bindings:** Rejected under ADR-0007 because the UnRAR license restricts competing RAR compression functionality and is incompatible with GPL-3.0-or-later static/dynamic linking.
- **Requiring OS power tools at compile time / skipping non-Linux inhibition:** Rejected because M9.3 explicitly targets Windows, Linux, and macOS CI and runtime parity.
- **Polling-only or custom raw socket FTP parser:** Using `suppaftp` handles EPSV/PASV, `AUTH TLS`, PBSZ/PROT, and `REST` across diverse FTP servers while keeping TLS on `rustls`.

## Consequences

- `ruscello-http` (or engine transfer path) handles `http`, `https`, `ftp`, and `ftps` schemes while keeping `.rscpart.ctl` recovery safe and secret-free.
- `ruscello-engine` adds `ruscello-platform`, `sevenz-rust2`, `zip`, `tar`, `flate2`, `lzma-rs`, `bzip2-rs`, and `chrono` while remaining strictly acyclic (`core` ← `platform` / `http` / `storage` ← `engine` ← `api` ← `apps/*`).
- Users on all three OSes get out-of-the-box extraction for `.zip`, `.7z`, and `.tar.*`; `.rar` works automatically whenever `7z`/`7zz`/`unrar` is installed or pointed to in Settings.
- All M9 features can be tested deterministically offline with injectable clocks, mock power controllers, loopback FTP/FTPS servers in `ruscello-testkit`, and temporary directories.
