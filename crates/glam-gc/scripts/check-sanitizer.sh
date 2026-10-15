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

sanitizer_test() {
  RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-Zsanitizer=$sanitizer" \
    cargo +nightly test \
      -Zbuild-std \
      --target "$target" \
      --package glam-gc \
      --all-features \
      --lib \
      "$@"
}

if [[ "$sanitizer" == address ]]; then
  ASAN_OPTIONS="detect_leaks=1" sanitizer_test
else
  sanitizer_test
fi
