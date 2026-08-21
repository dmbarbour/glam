#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/../../.." && pwd)"
cd "$root"

# C1's prototype allocator deliberately leaks every payload so pointer and
# trace contracts can be checked before arena ownership exists. Miri still
# checks provenance, aliasing, initialization, and invalid access; C2 must
# remove this exception when it replaces the prototype allocation path.
MIRIFLAGS="${MIRIFLAGS:+$MIRIFLAGS }-Zmiri-ignore-leaks" \
  cargo +nightly miri test --package glam-gc --lib --all-features
