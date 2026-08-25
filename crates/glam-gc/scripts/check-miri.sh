#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/../../.." && pwd)"
cd "$root"

# Arena-owned payloads and chunks are destroyed at terminal heap teardown.
# Miri's leak check therefore remains enabled as part of the allocator
# contract, in addition to strict provenance, aliasing, initialization, and
# access checking.
#
# One ownership fixture deliberately forgets an inert 24-byte class-frontier
# cell to prove that an escaped scoped allocator cannot retain its heap. Run
# that exact fixture separately without leak checking, matching the ASan gate,
# while keeping every other Miri check and the complete-suite leak check.
fixture="heap::tests::forgotten_scoped_allocator_does_not_retain_its_heap"
MIRIFLAGS="${MIRIFLAGS:+$MIRIFLAGS }-Zmiri-strict-provenance" \
  cargo +nightly miri test --package glam-gc --lib --all-features -- \
    --skip "$fixture"
MIRIFLAGS="${MIRIFLAGS:+$MIRIFLAGS }-Zmiri-strict-provenance -Zmiri-ignore-leaks" \
  cargo +nightly miri test --package glam-gc --lib --all-features \
    "$fixture" -- --exact
