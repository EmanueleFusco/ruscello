#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
seconds=${1:-60}
# Prebuilt cargo-fuzz binaries may default to their own target instead of the toolchain host.
host=$(rustc +nightly --version --verbose | sed -n 's/^host: //p')
for target in content_disposition hls dash recipe checkpoint; do
  corpus="fuzz/corpus/$target"
  mkdir -p "$corpus"
  if [[ -d fuzz/seeds/$target ]]; then
    for seed in "fuzz/seeds/$target/"*; do
      [[ -e "$corpus/${seed##*/}" ]] || cp "$seed" "$corpus/"
    done
  fi
  cargo +nightly fuzz run --target "$host" "$target" "$corpus" -- \
    -max_total_time="$seconds" -max_len=65536 -rss_limit_mb=2048
done
