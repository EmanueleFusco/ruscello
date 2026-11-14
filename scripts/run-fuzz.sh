#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
seconds=${1:-60}
for target in content_disposition hls dash recipe checkpoint; do
  corpus="fuzz/corpus/$target"
  mkdir -p "$corpus"
  if [[ -d fuzz/seeds/$target ]]; then
    cp -n "fuzz/seeds/$target/"* "$corpus/" || true
  fi
  cargo +nightly fuzz run "$target" "$corpus" -- -max_total_time="$seconds" -max_len=65536 -rss_limit_mb=2048
done
