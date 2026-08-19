#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/../../.." && pwd)"
cd "$root"

RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-Funsafe-code" \
  cargo check --package glam-gc --all-targets --all-features
