#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/../../.." && pwd)"
cd "$root"

# Keep million-edge evidence independently measurable rather than adding its
# allocation and traversal cost to every ordinary unit-test run.
cargo test --package glam-gc --all-features --lib c5d_scale_ -- \
  --ignored --nocapture --test-threads=1
