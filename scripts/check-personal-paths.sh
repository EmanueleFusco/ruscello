#!/usr/bin/env bash
# Scan text tracked by Git, including new files staged for this change.
set -euo pipefail
cd "$(dirname "$0")/.."
# Exclude dollar-prefixed placeholders and generic system directories.
pattern='(/(home|Users)/[[:alnum:]_.-]+/|[A-Za-z]:[\\/]+[Uu]sers[\\/]+[[:alnum:]_.-]+[\\/])'
if git grep -nIE "$pattern" -- .; then
  echo "Replace absolute paths under a personal home directory." >&2
  exit 1
else
  status=$?
  test "$status" -eq 1
fi
