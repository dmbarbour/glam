#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/../../.." && pwd)"
cd "$root"

# Arena-owned payloads and chunks are destroyed at terminal heap teardown.
# Miri's leak check therefore remains enabled as part of the allocator
# contract, in addition to provenance, aliasing, initialization, and access
# checking.
cargo +nightly miri test --package glam-gc --lib --all-features
