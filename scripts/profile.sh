#!/usr/bin/env bash
# Runs the glam-prof profiling workloads against a release build.
#
#   scripts/profile.sh [OUT_DIR] [WORKLOAD...]
#
# Builds `glam` with the `glam-prof` feature in release mode, writes each
# workload's JSON report to OUT_DIR (default target/profile/<timestamp>),
# and prints a summary table. Requires python3 for the table. With
# WORKLOAD arguments, runs only those workloads.
#
# Counters are exact and comparable between runs; timings are trend data,
# easily disturbed by other load on the machine. Where `perf` can count
# user-space instructions, each workload also reports them, nearly as
# stable as the counters, and its CPU time, kernel included. Instructions
# miss kernel work such as futex wake-ups; CPU time includes it but varies
# with load. GLAM_PROFILE_REPEAT=N averages N runs of each workload. Record baselines in the relevant plan or
# performance review.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

out="${1:-target/profile/$(date +%Y%m%d-%H%M%S)}"
shift || true
mkdir -p "$out/workloads"

cargo build --release --features glam-prof --bin glam -q
glam="$root/target/release/glam"

# A rootless container needs the host's kernel.perf_event_paranoid at 2 or
# less for this.
counter=()
if command -v perf >/dev/null &&
  perf stat -x, -e instructions:u -o /dev/null true 2>/dev/null; then
  counter=(perf stat -x, -e instructions:u,task-clock -r "${GLAM_PROFILE_REPEAT:-1}" -o)
else
  printf 'perf cannot count instructions here; reporting times only\n' >&2
fi

# Generated sources. Keep each workload small enough to finish in seconds;
# the parser and recursion workloads are exponential or superlinear today.
gen() { printf '%s\n' "$2" >"$out/workloads/$1.g"; }

gen minimal 'language g0
asm.result = "ok"'

parens() {
  local depth=$1 open='' close=''
  for ((i = 0; i < depth; i++)); do open+='('; close+=')'; done
  printf 'language g0\nx = %s1%s\nasm.result = "ok"' "$open" "$close"
}
lists() {
  local depth=$1 open='' close=''
  for ((i = 0; i < depth; i++)); do open+='['; close+=']'; done
  printf 'language g0\nx = %s1%s\nasm.result = "ok"' "$open" "$close"
}
ifs() {
  local depth=$1 open='' close=''
  for ((i = 0; i < depth; i++)); do open+='(if c then '; close+=' else 0)'; done
  printf 'language g0\nc = 1 == 1\nx = %s1%s\nasm.result = "ok"' "$open" "$close"
}
gen parse_parens_10 "$(parens 10)"
gen parse_lists_16 "$(lists 16)"
gen parse_ifs_10 "$(ifs 10)"

countdown() {
  printf 'language g0\nloop n = if n == 0 then 0 else loop (n - 1)\nasm.result = if loop %s == 0 then "ok" else "bad"' "$1"
}
gen countdown_100 "$(countdown 100)"
gen countdown_200 "$(countdown 200)"
gen countdown_400 "$(countdown 400)"

list_map() {
  # Comparing with the expected list forces every mapped element; `len`
  # alone would force only the spine.
  local n=$1 items expected
  items=$(seq -s ', ' 1 "$n")
  expected=$(seq -s ', ' 2 "$((n + 1))")
  printf 'language g0\nimport '"'"'std as std\nxs = [%s]\nys = std.list.map (\\x -> x + 1) xs\nasm.result = if ys == [%s] then "ok" else "bad"' "$items" "$expected"
}
gen list_map_1000 "$(list_map 1000)"

dict_lookup() {
  local n=$1 entries='' i
  for ((i = 1; i <= n; i++)); do entries+="k$i:$i,"; done
  entries=${entries%,}
  printf 'language g0\nd = {%s}\nasm.result = if d.k%s == %s then "ok" else "bad"' "$entries" "$n" "$n"
}
gen dict_lookup_1000 "$(dict_lookup 1000)"

# Each workload: name, GLAM_CONF, source file, and the start of its expected
# output, so a broken workload cannot produce a bogus baseline.
declare -a names=() confs=() files=() expects=()
add() { names+=("$1"); confs+=("$2"); files+=("$3"); expects+=("$4"); }
add hello_elf samples/config/direct_assembly.g \
  samples/executable/hello_x86_64_linux/hello.g $'\x7fELF'
add hello_do '' samples/hello/hello_do.g 'Hello, World!'
for name in minimal parse_parens_10 parse_lists_16 parse_ifs_10 countdown_100 countdown_200 \
  countdown_400 list_map_1000 dict_lookup_1000; do
  add "$name" '' "$out/workloads/$name.g" ok
done

selected=("$@")
wanted() {
  [[ ${#selected[@]} -eq 0 ]] && return 0
  local name
  for name in "${selected[@]}"; do [[ $name == "$1" ]] && return 0; done
  return 1
}

for index in "${!names[@]}"; do
  name=${names[$index]}
  wanted "$name" || continue
  printf 'running %s\n' "$name" >&2
  run=("$glam" --file "${files[$index]}")
  if [[ ${#counter[@]} -gt 0 ]]; then
    run=("${counter[@]}" "$out/$name.perf" "${run[@]}")
  fi
  if ! GLAM_CONF="${confs[$index]}" GLAM_PROF="$out/$name.json" \
    "${run[@]}" >"$out/$name.out" 2>"$out/$name.err"; then
    printf '  %s failed; see %s\n' "$name" "$out/$name.err" >&2
  fi
  expected=${expects[$index]}
  if [[ "$(head -c "${#expected}" "$out/$name.out")" != "$expected" ]]; then
    printf '  %s produced unexpected output; see %s\n' "$name" "$out/$name.out" >&2
    rm -f "$out/$name.json"
  fi
done

python3 - "$out" <<'EOF'
import json, pathlib, sys

out = pathlib.Path(sys.argv[1])
rows = []
for path in sorted(out.glob("*.json")):
    report = json.loads(path.read_text())
    runtime = report["runtime"]
    reductions = sum(runtime["reductions"].values()) + sum(runtime["net_reductions"].values())
    heap = runtime["heap"] or {}
    # perf's CSV lines: count, unit, event, variance, ...
    instructions = cpu = None
    counts = path.with_suffix(".perf")
    if counts.exists():
        for line in counts.read_text().splitlines():
            fields = line.split(",")
            if len(fields) < 3:
                continue
            try:
                value = float(fields[0])
            except ValueError:
                continue
            if fields[2].startswith("instructions"):
                instructions = value / 1e6
            elif fields[2].startswith("task-clock"):
                cpu = value
    rows.append((
        path.stem,
        instructions,
        cpu,
        report["phases_ns"]["total"] / 1e6,
        reductions,
        heap.get("outer_access_regions", 0),
        heap.get("root_registrations", 0),
        heap.get("allocations", 0),
        runtime["phases"]["collections"],
    ))
header = ("workload", "M instr", "cpu ms", "total ms", "reductions", "access", "roots", "allocs", "GCs")
print(f"{header[0]:<18}{header[1]:>10}{header[2]:>9}{header[3]:>11}{header[4]:>12}{header[5]:>10}{header[6]:>10}{header[7]:>10}{header[8]:>5}")
for row in rows:
    instructions = "-" if row[1] is None else f"{row[1]:.1f}"
    cpu = "-" if row[2] is None else f"{row[2]:.1f}"
    print(f"{row[0]:<18}{instructions:>10}{cpu:>9}{row[3]:>11.1f}{row[4]:>12}{row[5]:>10}{row[6]:>10}{row[7]:>10}{row[8]:>5}")
print(f"\nreports: {out}")
EOF
