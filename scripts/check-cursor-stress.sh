#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

# Keep the 1,100-layer recursion-limit and productive-materialization proofs
# independently measurable rather than charging them to every routine test.
cargo test --lib cursor_stress_ -- \
  --ignored --nocapture --test-threads=1
