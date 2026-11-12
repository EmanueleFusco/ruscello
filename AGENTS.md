# AGENTS.md — Instructions for Agents Working on Ruscello

Ruscello is a desktop download manager written **entirely in Rust** that combines a BitTorrent client
(qBittorrent-style) and an HTTP/hoster download manager (JDownloader/IDM-style).

Before writing any code, read these documents in order:
1. `docs/PLAN.md` — roadmap, milestones, tasks with checkboxes, Definition of Done (**single source of truth**).
2. `docs/ARCHITECTURE.md` — crates, responsibilities, data flows, recommended external crates.
3. `docs/adr/` — Architecture Decision Records already agreed upon (do not overturn them without a new ADR).
4. `docs/UI.md` — user interface specifications.
5. `docs/TESTING.md` — testing strategy and mandatory CI gates.
6. `docs/reference/legacy-scripts/README.md` — real-world owner use cases that must be supported.

**Language Policy (Mandatory):**
- **Development & Repository:** All repository files, documentation, ADRs, code, identifiers, code comments, commit messages, issue descriptions, and PR titles/descriptions **must be written in English**.
- **Application UI (i18n):** The application itself **must be available in both English (`en`) and Italian (`it`)**, with a user-selectable language setting in the Preferences (defaulting to the system locale when supported, with runtime switching).

---

## 1. Non-Negotiable Rules

1. **All Rust.** Both the GUI (`iced`) and the backend must be written in Rust. Permitted exceptions already agreed upon:
   - browser extension (`extensions/browser`): minimal JS/TS required by WebExtension APIs;
   - **optional external sidecars** downloaded by the app with user consent: `yt-dlp`, `ffmpeg`,
     and optionally `7z`/`unrar`. The application must work without them.
   No Electron/Tauri-with-web-frontend, no Python, no Node.js in the desktop application.
2. **No automated bypassing of anti-bot challenges.** Do not implement CAPTCHA solvers, automated clicks on Turnstile/reCAPTCHA/hCaptcha widgets, paid solving services, TLS/fingerprint spoofing (JA3), or "undetected/stealth" browsers. Challenges are **presented to the user** in the browser helper and the resulting session is reused (ADR-0004). Mandatory hoster countdowns must be respected.
3. **Never corrupt or lose downloads.** Any change to `ruscello-http`, the writer, the control file, or storage must keep crash/resume tests (process kill) green and preserve the invariant:
   *fsync data first → update control file second*.
4. **Green CI is mandatory** before merging: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (or `cargo nextest run`) on Windows, Linux, and macOS, and `cargo deny check`.
5. **No default test may depend on the live network.** Live tests must be gated behind the `live-tests` feature and/or `#[ignore]`.
6. **License: GPL-3.0-or-later.** All dependencies must have compatible licenses (enforced by `cargo-deny`). Code adapted from other projects must be attributed in the file/comment and carry a compatible license.
7. **No secrets in the repository** (tokens, cookies, accounts). User credentials must be stored exclusively in the OS keyring.
8. **No `unwrap()`/`expect()` on runtime code paths** (allowed in tests and for provable invariants documented with an `// INVARIANT:` comment). `unsafe` is forbidden except for documented OS FFI with a `// SAFETY:` comment and justification in the PR.
9. **English for all development artifacts; English + Italian for the user-facing application.** Every new user-facing string in the GUI/CLI must go through the localization system (`en` and `it` bundles kept in sync), and the user must be able to switch the application language in Settings.

## 2. Commands

```powershell
cargo check --workspace
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                 # or: cargo nextest run --workspace
cargo deny check                       # licenses, advisories, duplicates
cargo run -p ruscello-gui              # GUI
cargo run -p ruscello-cli -- get <url> # headless engine CLI
./scripts/check.ps1                    # all local checks (Windows) — ./scripts/check.sh on Linux/macOS
```

Linux system packages required to build the browser helper (once `wry` is added):
`libwebkit2gtk-4.1-dev libsoup-3.0-dev libgtk-3-dev libxdo-dev` (+ `libayatana-appindicator3-dev` for the system tray).

## 3. Repository Structure

```
apps/
  gui/        ruscello-gui      main desktop binary (iced)
  cli/        ruscello-cli      CLI/headless runner, also used in crash-resume tests
  webview/    ruscello-webview  browser helper process (wry/tao), spawned on demand
crates/
  core/       ruscello-core     domain types, state machine, events, shared errors (no I/O)
  http/       ruscello-http     segmented HTTP/FTP engine, writer, control file, rate limiter
  torrent/    ruscello-torrent  wrapper around librqbit
  media/      ruscello-media    HLS/DASH, optional yt-dlp/ffmpeg sidecar management
  resolver/   ruscello-resolver hoster/crawler plugins, TOML recipes, LinkGrabber & scoring
  browser/    ruscello-browser  protocol + client for the webview helper process
  storage/    ruscello-storage  SQLite, migrations, settings
  engine/     ruscello-engine   orchestrator: queue, scheduler, EngineHandle, post-processing
  api/        ruscello-api      local API for browser extension/CLI, single-instance IPC
  testkit/    ruscello-testkit  local test server and utilities (dev-dependency only)
extensions/browser/             MV3 Chrome/Firefox extension (M8)
docs/                           plan, architecture, ADRs, UI, testing, references
```

Dependency direction (no cycles): `core` ← `http`/`torrent`/`media`/`resolver`/`browser`/`storage`
← `engine` ← `api` ← `apps/*`. The GUI depends only on `engine` (+ `core`), never directly on `http`/`torrent`.

## 4. Workflow

- **One milestone at a time**, following the order in `docs/PLAN.md` (M4/M5/M7 can be parallelized after M3).
- For each task: GitHub Issue → branch `feat/mX-short-description` → small commits (Conventional Commits in English) →
  PR with description, DoD checklist, and test summary → green CI → squash merge into `main`.
  Use `gh` (`gh issue list --milestone "M1 ..."`, `gh pr create`, `gh pr merge --squash`).
- Update checkboxes in `docs/PLAN.md` in the same PR that completes a task; update `CHANGELOG.md`
  (under *Unreleased*).
- New architectural decisions or changes to existing ones → new file `docs/adr/NNNN-title.md`
  (sections: Context, Decision, Alternatives, Consequences) written in English.
- If a requirement is ambiguous or a decision impacts the user (UX, licensing, privacy, heavy dependencies),
  **ask the project owner** instead of guessing; you have freedom over internal implementation details.

### Where You Have Freedom
Choice of specific crates (beyond those fixed in ADRs), internal module layout, internal algorithms
(as long as invariants and tests hold), visual details within `docs/UI.md`, internal binary format of
control files, and test organization. Crates listed in `docs/ARCHITECTURE.md` are recommendations:
always verify the latest version on crates.io and maintenance status.

### What Not to Do
- Do not introduce unrequested "drive-by" work (massive refactors, renames) mixed into feature PRs.
- Do not disable lints or tests to make CI pass; do not use `#[allow(...)]` without an explanatory comment.
- Do not add heavy dependencies without justifying them in the PR (binary size, build time).
- Do not make live network calls in default tests; do not commit large binary files (generate fixtures programmatically).
- Do not write documentation, comments, commit messages, or PRs in languages other than English.

## 5. Definition of Done (Applies to Every PR)

- [ ] Compiles without warnings on Windows/Linux/macOS (CI).
- [ ] New tests for new behavior (unit + integration where appropriate); existing tests pass.
- [ ] No regression against the resource budget (`docs/PLAN.md` §2) for touched components.
- [ ] Errors handled and presented to the user clearly (no panics, no raw internal error dumps).
- [ ] Useful `tracing` logs (`debug` level for details, no sensitive data: cookies/tokens masked).
- [ ] User-facing strings localized in both English (`en`) and Italian (`it`).
- [ ] `docs/PLAN.md` and `CHANGELOG.md` updated; ADR added if an architectural decision was made.

## 6. Coding Conventions

- Edition 2024, stable toolchain (`rust-toolchain.toml`). Workspace lints configured in `Cargo.toml`.
- Errors: `thiserror` in library crates (per-crate error enums), `anyhow` in binary crates only.
- Async: `tokio`. Never block the async runtime: heavy file I/O must use dedicated tasks or `spawn_blocking`.
- Concurrency: prefer actors (tasks + channels) over shared `Mutex`; never hold a lock across an `.await`.
- Cancellation: propagate `tokio_util::sync::CancellationToken` across all download tasks.
- Abstract time and network where needed for deterministic testing (e.g., injectable clock for scheduler and rate limiter).
- Filenames/paths: handle Unicode, Windows long paths (`\\?\`), and OS-forbidden characters.
- The GUI performs no I/O: it sends commands to `EngineHandle` and reacts to events.
