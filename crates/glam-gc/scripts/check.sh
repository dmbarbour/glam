#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/../../.." && pwd)"
cd "$root"

cargo fmt --check --package glam-gc
cargo clippy --package glam-gc --all-targets --all-features -- -D warnings
cargo test --package glam-gc --all-features
crates/glam-gc/scripts/audit-unsafe.sh
