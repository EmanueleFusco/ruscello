#!/usr/bin/env bash
# Run all local checks (same gates as CI).
set -euo pipefail
cd "$(dirname "$0")/.."

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
if command -v cargo-nextest >/dev/null 2>&1; then
  cargo nextest run --workspace --no-tests=pass
  cargo test --workspace --doc
else
  cargo test --workspace
fi
if command -v cargo-deny >/dev/null 2>&1; then
  cargo deny check
else
  echo "cargo-deny not installed (cargo install cargo-deny) - skipping"
fi
