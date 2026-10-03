#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/../../.." && pwd)"
cd "$root"

output="${1:-target/glam-gc-c8-measurements.jsonl}"
mkdir -p "$(dirname "$output")"
temporary="${output}.tmp.$$"
trap 'rm -f "$temporary"' EXIT

GLAM_GC_MEASUREMENT_REVISION="$(git rev-parse HEAD)" \
  cargo run --quiet --release --package glam-gc \
    --features deterministic-test-hooks --example c8_measurements -- all >"$temporary"
mv "$temporary" "$output"
trap - EXIT
printf 'wrote %s\n' "$output"
