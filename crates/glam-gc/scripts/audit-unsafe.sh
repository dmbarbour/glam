#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/../../.." && pwd)"
cd "$root"

actual_sites="$(mktemp)"
actual_modules="$(mktemp)"
trap 'rm -f "$actual_sites" "$actual_modules"' EXIT

rg --with-filename --no-line-number \
  'unsafe[[:space:]]+(fn|impl)|unsafe[[:space:]]*\{' \
  crates/glam-gc/src -g '*.rs' \
  | grep -v ':[[:space:]]*///' \
  | sort >"$actual_sites"

rg --with-filename --no-line-number \
  '^#\[(allow|expect)\(unsafe_code.*\)\]$' \
  crates/glam-gc/src -g '*.rs' \
  | sort >"$actual_modules"

diff -u crates/glam-gc/scripts/unsafe-sites.txt "$actual_sites"
diff -u crates/glam-gc/scripts/unsafe-modules.txt "$actual_modules"

# The crate-level deny ensures that an unsafe construct outside one of the
# explicitly listed modules remains a compiler error as well as an inventory
# mismatch.
cargo check --package glam-gc --all-targets --all-features
