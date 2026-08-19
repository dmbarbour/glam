#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 address|thread" >&2
  exit 2
fi

case "$1" in
  address|thread) sanitizer="$1" ;;
  *)
    echo "unsupported sanitizer: $1" >&2
    exit 2
    ;;
esac

root="$(cd "$(dirname "$0")/../../.." && pwd)"
cd "$root"
target="$(rustc +nightly -vV | sed -n 's/^host: //p')"

RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-Zsanitizer=$sanitizer" \
  cargo +nightly test \
    -Zbuild-std \
    --target "$target" \
    --package glam-gc \
    --all-features
