#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/../../.." && pwd)"
cd "$root"

cargo +nightly miri test --package glam-gc --lib --all-features
