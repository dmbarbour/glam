#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/../../.." && pwd)"
cd "$root"

runs="${GLAM_BASELINE_RUNS:-7}"
case "$runs" in
  ''|*[!0-9]*)
    echo "GLAM_BASELINE_RUNS must be a positive integer" >&2
    exit 2
    ;;
esac
if [[ "$runs" -eq 0 ]]; then
  echo "GLAM_BASELINE_RUNS must be a positive integer" >&2
  exit 2
fi

if [[ "$(uname -s)" != Linux ]]; then
  echo "the G0 peak-RSS baseline currently supports Linux only" >&2
  exit 2
fi
if ! command -v python3 >/dev/null; then
  echo "the G0 baseline requires Python 3 for timing and getrusage" >&2
  exit 2
fi

cargo build --release --bin glam
binary="$root/target/release/glam"

measure() {
  local label="$1"
  local configuration="$2"
  shift 2

  GLAM_CONF="$configuration" env -u GLAM_WORKERS python3 - \
    "$label" "$runs" "$binary" "$@" <<'PY'
import os
import resource
import statistics
import subprocess
import sys
import time

label = sys.argv[1]
runs = int(sys.argv[2])
command = sys.argv[3:]
timings_ms = []
output_bytes = None

for attempt in range(runs + 1):
    started = time.perf_counter_ns()
    completed = subprocess.run(
        command,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=os.environ,
    )
    elapsed_ms = (time.perf_counter_ns() - started) / 1_000_000
    if completed.returncode != 0:
        sys.stderr.write(
            f"{label} failed with exit {completed.returncode}\n"
            f"stderr:\n{completed.stderr.decode(errors='replace')}"
        )
        raise SystemExit(completed.returncode)
    if completed.stderr:
        sys.stderr.write(
            f"{label} unexpectedly wrote stderr:\n"
            f"{completed.stderr.decode(errors='replace')}"
        )
        raise SystemExit(1)
    if output_bytes is None:
        output_bytes = len(completed.stdout)
    elif output_bytes != len(completed.stdout):
        raise SystemExit(f"{label} output length changed between repetitions")
    if attempt != 0:
        timings_ms.append(elapsed_ms)

# On Linux ru_maxrss is KiB. Because this process launches only the measured
# children, this is the maximum child RSS across the warmup and measured runs.
peak_rss_kib = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
print(
    f"| {label} | {runs} | {statistics.median(timings_ms):.3f} | "
    f"{min(timings_ms):.3f} | {max(timings_ms):.3f} | "
    f"{peak_rss_kib} | {output_bytes} |"
)
PY
}

echo "Glam G0 operational baseline"
echo "revision: $(git rev-parse HEAD)"
echo "rustc: $(rustc --version)"
echo "host: $(uname -srmo)"
echo "logical processors: $(nproc)"
echo "release binary bytes: $(stat -c %s "$binary")"
echo
echo "| workload | measured runs | median ms | min ms | max ms | peak RSS KiB | stdout bytes |"
echo "| --- | ---: | ---: | ---: | ---: | ---: | ---: |"

measure \
  hello_dict_w0 \
  samples/config/unit_tests.g \
  --workers 0 --file samples/hello/hello_dict.g
measure \
  ordered_mixins_w0 \
  samples/config/unit_tests.g \
  --workers 0 \
  --file samples/assembly/mixin_override.g \
  --file samples/assembly/mixin_base.g
measure \
  direct_assembly_elf_w0 \
  samples/config/direct_assembly.g \
  --workers 0 \
  --file samples/executable/hello_x86_64_linux/hello.g
measure \
  hello_dict_w4 \
  samples/config/unit_tests.g \
  --workers 4 \
  --file samples/hello/hello_dict.g
