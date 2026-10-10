#!/usr/bin/env bash
# Runs the glam-prof profiling workloads against a release build.
#
#   scripts/profile.sh [OUT_DIR] [WORKLOAD|FAMILY...]
#
# Builds `glam` with the `glam-prof` feature in release mode, writes each
# workload's JSON report to OUT_DIR (default target/profile/<timestamp>),
# and prints a summary table. Requires python3 for the table. With
# arguments, runs only the named workloads, or every size of a named
# family.
#
# Most workloads come in families run at three sizes, n, 2n and 4n, because
# a cost that grows superlinearly looks like a large constant at any one
# size. The summary's growth table fits each cost to a + b·n^k over the
# three sizes and reports k: 1 is linear, 2 quadratic. Fixed costs cancel.
# GLAM_PROFILE_SCALE=N multiplies every family's sizes, for a closer look at
# large inputs. A family named <name>_w<N>, such as chain_w4, runs with N
# background workers; every other workload runs with none.
#
# Counters are exact and comparable between runs; timings are trend data,
# easily disturbed by other load on the machine. Where `perf` can count
# user-space instructions, each workload also reports them, nearly as
# stable as the counters, and its CPU time, kernel included. Instructions
# miss kernel work such as futex wake-ups; CPU time includes it but varies
# with load. GLAM_PROFILE_REPEAT=N averages N runs of each workload.
# GLAM_PROFILE_TIMEOUT (seconds, default 120) stops a runaway workload; a
# timeout is itself a finding. Record baselines in the relevant plan.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

out="${1:-target/profile/$(date +%Y%m%d-%H%M%S)}"
shift || true
mkdir -p "$out/workloads"

cargo build --release --features glam-prof --bin glam -q
glam="$root/target/release/glam"
scale=${GLAM_PROFILE_SCALE:-1}
limit=${GLAM_PROFILE_TIMEOUT:-120}

# A rootless container needs the host's kernel.perf_event_paranoid at 2 or
# less for this.
counter=()
if command -v perf >/dev/null &&
  perf stat -x, -e instructions:u -o /dev/null true 2>/dev/null; then
  counter=(perf stat -x, -e instructions:u,task-clock -r "${GLAM_PROFILE_REPEAT:-1}" -o)
else
  printf 'perf cannot count instructions here; reporting times only\n' >&2
fi

# Each workload: name, GLAM_CONF, source file, the start of its expected
# output, so a broken workload cannot produce a bogus baseline, and its
# background worker count (default none).
declare -a names=() confs=() files=() expects=() workers=()
add() { names+=("$1"); confs+=("$2"); files+=("$3"); expects+=("$4"); workers+=("${5:-0}"); }

# A generated workload with a fixed source, and optionally a worker count.
gen() {
  printf '%s\n' "$2" >"$out/workloads/$1.g"
  add "$1" '' "$out/workloads/$1.g" ok "${3:-0}"
}

# A family: the generator function of the same name, run at n, 2n and 4n.
# A third argument runs it with that many background workers, as the family
# <name>_w<workers>.
family() {
  local name=$1 base=$2 count=${3:-0} label=$1 factor size
  if [[ $count -gt 0 ]]; then label=${name}_w$count; fi
  for factor in 1 2 4; do
    size=$((base * scale * factor))
    gen "${label}_$size" "$("$name" "$size")" "$count"
  done
}

add hello_elf samples/config/direct_assembly.g \
  samples/executable/hello_x86_64_linux/hello.g $'\x7fELF'
add hello_do '' samples/hello/hello_do.g 'Hello, World!'
gen minimal 'language g0
asm.result = "ok"'

# Front end only: the nested or large definition is never demanded.
nest() {
  local depth=$1 open=$2 close=$3 opens='' closes='' i
  for ((i = 0; i < depth; i++)); do opens+=$open; closes+=$close; done
  printf '%s1%s' "$opens" "$closes"
}
parse_parens() { printf 'language g0\nx = %s\nasm.result = "ok"' "$(nest "$1" '(' ')')"; }
parse_lists() { printf 'language g0\nx = %s\nasm.result = "ok"' "$(nest "$1" '[' ']')"; }
parse_ifs() {
  printf 'language g0\nc = 1 == 1\nx = %s\nasm.result = "ok"' \
    "$(nest "$1" '(if c then ' ' else 0)')"
}
list_literal() { printf 'language g0\nxs = [%s]\nasm.result = "ok"' "$(seq -s ', ' 1 "$1")"; }
family parse_parens 100
family parse_lists 100
# Exponential in depth today; see the structural overheads plan.
family parse_ifs 4
family list_literal 1000

# Recursion: in tail position, not in tail position, and through a chain
# of module definitions.
countdown() {
  printf 'language g0\nloop n = if n == 0 then 0 else loop (n - 1)\nasm.result = if loop %s == 0 then "ok" else "bad"' "$1"
}
sum() {
  printf 'language g0\nsum n = if n == 0 then 0 else n + sum (n - 1)\nasm.result = if sum %s == %s then "ok" else "bad"' \
    "$1" "$(($1 * ($1 + 1) / 2))"
}
chain() {
  local n=$1 i
  printf 'language g0\nx1 = 1\n'
  for ((i = 2; i <= n; i++)); do printf 'x%d = x%d + 1\n' "$i" "$((i - 1))"; done
  printf 'asm.result = if x%d == %d then "ok" else "bad"' "$n" "$n"
}
family countdown 200
family sum 200
family chain 200
# Workers follow the same producer chain as the foreground, so a scheduling
# defect shows as superlinear growth here (`perf-worker-scaling`).
family chain 200 4

# Lists and dicts.
list_map() {
  # Comparing with the expected list forces every mapped element; `len`
  # alone would force only the spine.
  local n=$1 items expected
  items=$(seq -s ', ' 1 "$n")
  expected=$(seq -s ', ' 2 "$((n + 1))")
  printf 'language g0\nimport '"'"'std as std\nxs = [%s]\nys = std.list.map (\\x -> x + 1) xs\nasm.result = if ys == [%s] then "ok" else "bad"' "$items" "$expected"
}
list_computed() {
  # A literal whose items are computed, so it builds at run time rather
  # than resolving to its value; `len` forces only the spine.
  printf 'language g0\nimport '"'"'std as std\nxs = [%s]\nasm.result = if std.list.len xs == %s then "ok" else "bad"' \
    "$(seq 1 "$1" | sed 's/$/ + 0/' | paste -sd, - | sed 's/,/, /g')" "$1"
}
list_sum() {
  # Walks the list from the front, as a fold would.
  printf 'language g0\nimport '"'"'std as std\nxs = [%s]\ngo acc ys = if std.list.len ys == 0 then acc else go (acc + std.list.head ys) (std.list.tail ys)\nasm.result = if go 0 xs == %s then "ok" else "bad"' \
    "$(seq -s ', ' 1 "$1")" "$(($1 * ($1 + 1) / 2))"
}
append_walk() {
  # A list the program builds by appending one item at a time is a
  # left-deep spine, unlike a literal; the walk then takes it from the
  # front. `ys == []` pops one item, where `len` would count them all.
  printf 'language g0\nimport '"'"'std as std\nbuild n = if n == 0 then [] else build (n - 1) ++ [n]\ngo acc ys = if ys == [] then acc else go (acc + std.list.head ys) (std.list.tail ys)\nasm.result = if go 0 (build %s) == %s then "ok" else "bad"' \
    "$1" "$(($1 * ($1 + 1) / 2))"
}
dict_computed() {
  # A literal whose values are computed, so it builds at run time rather
  # than resolving to its dictionary.
  local n=$1 entries='' i
  for ((i = 1; i <= n; i++)); do entries+="k$i:$i + 0,"; done
  entries=${entries%,}
  printf 'language g0\nd = {%s}\nasm.result = if d.k%s == %s then "ok" else "bad"' "$entries" "$n" "$n"
}
dict_lookup() {
  local n=$1 entries='' i
  for ((i = 1; i <= n; i++)); do entries+="k$i:$i,"; done
  entries=${entries%,}
  printf 'language g0\nd = {%s}\nasm.result = if d.k%s == %s then "ok" else "bad"' "$entries" "$n" "$n"
}
family list_map 500
family list_computed 500
family dict_computed 500
family list_sum 100
family append_walk 200
family dict_lookup 500

# A long effect chain: one `do` block binding each step's result.
do_chain() {
  local n=$1 i
  printf 'language g0\nimport '"'"'std\nmessage = do\n  a1 <- .r 1\n'
  for ((i = 2; i <= n; i++)); do printf '  a%d <- .r (a%d + 1)\n' "$i" "$((i - 1))"; done
  printf '  .r a%d\nasm.result = if list.head (list.pure message) == %d then "ok" else "bad"' "$n" "$n"
}
family do_chain 200

selected=("$@")
wanted() {
  [[ ${#selected[@]} -eq 0 ]] && return 0
  local name
  for name in "${selected[@]}"; do
    [[ $name == "$1" ]] && return 0
    [[ $1 =~ ^(.*)_[0-9]+$ && ${BASH_REMATCH[1]} == "$name" ]] && return 0
  done
  return 1
}

for index in "${!names[@]}"; do
  name=${names[$index]}
  wanted "$name" || continue
  printf 'running %s\n' "$name" >&2
  run=(timeout "$limit" "$glam" --file "${files[$index]}")
  if [[ ${#counter[@]} -gt 0 ]]; then
    run=("${counter[@]}" "$out/$name.perf" "${run[@]}")
  fi
  status=0
  GLAM_CONF="${confs[$index]}" GLAM_PROF="$out/$name.json" GLAM_WORKERS="${workers[$index]}" \
    "${run[@]}" >"$out/$name.out" 2>"$out/$name.err" || status=$?
  if [[ $status -eq 124 ]]; then
    printf '  %s timed out after %s s\n' "$name" "$limit" >&2
  elif [[ $status -ne 0 ]]; then
    printf '  %s failed; see %s\n' "$name" "$out/$name.err" >&2
  fi
  expected=${expects[$index]}
  if [[ "$(head -c "${#expected}" "$out/$name.out")" != "$expected" ]]; then
    [[ $status -eq 124 ]] ||
      printf '  %s produced unexpected output; see %s\n' "$name" "$out/$name.out" >&2
    rm -f "$out/$name.json"
  fi
done

python3 - "$out" <<'EOF'
import json, math, pathlib, re, sys

out = pathlib.Path(sys.argv[1])
rows = []
for path in out.glob("*.json"):
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
    sized = re.fullmatch(r"(.*)_(\d+)", path.stem)
    family, size = (sized[1], int(sized[2])) if sized else (path.stem, None)
    rows.append({
        "name": path.stem,
        "family": family,
        "size": size,
        "M instr": instructions,
        "cpu ms": cpu,
        "total ms": report["phases_ns"]["total"] / 1e6,
        "reductions": reductions,
        "access": heap.get("outer_access_regions", 0),
        "roots": heap.get("root_registrations", 0),
        "allocs": heap.get("allocations", 0),
        "GCs": runtime["phases"]["collections"],
    })
rows.sort(key=lambda row: (row["family"], row["size"] or 0))

header = ("workload", "M instr", "cpu ms", "total ms", "reductions", "access", "roots", "allocs", "GCs")
print(f"{header[0]:<20}{header[1]:>10}{header[2]:>9}{header[3]:>11}{header[4]:>12}{header[5]:>10}{header[6]:>10}{header[7]:>10}{header[8]:>5}")
for row in rows:
    instructions = "-" if row["M instr"] is None else f"{row['M instr']:.1f}"
    cpu = "-" if row["cpu ms"] is None else f"{row['cpu ms']:.1f}"
    print(f"{row['name']:<20}{instructions:>10}{cpu:>9}{row['total ms']:>11.1f}{row['reductions']:>12}"
          f"{row['access']:>10}{row['roots']:>10}{row['allocs']:>10}{row['GCs']:>5}")

def exponent(sizes, costs):
    """k with cost = a + b·size^k through three points, or a note."""
    (s1, s2, s3), (c1, c2, c3) = sizes, costs
    if None in costs:
        return "-"
    d1, d2 = c2 - c1, c3 - c2
    # Growth within noise, or none at all.
    if d2 <= 0.02 * abs(c3) or d1 <= 0:
        return "flat"
    target = d2 / d1
    ratio = lambda k: (s3**k - s2**k) / (s2**k - s1**k)
    low, high = 0.01, 8.0
    if target >= ratio(high):
        return f">{high:.0f}"
    if target <= ratio(low):
        return "<0.1"
    for _ in range(60):
        middle = (low + high) / 2
        low, high = (middle, high) if ratio(middle) < target else (low, middle)
    return f"{low:.2f}"

families = {}
for row in rows:
    if row["size"] is not None:
        families.setdefault(row["family"], []).append(row)
metrics = ("M instr", "cpu ms", "reductions", "access", "roots", "allocs")
print("\ngrowth exponent k, with cost ≈ a + b·n^k over the three largest sizes")
print(f"{'family':<16}{'sizes':>18}" + "".join(f"{metric:>12}" for metric in metrics) + f"{'K instr/n':>12}")
for family, members in families.items():
    if len(members) < 3:
        continue
    members = members[-3:]
    sizes = [row["size"] for row in members]
    cells = [exponent(sizes, [row[metric] for row in members]) for metric in metrics]
    # The marginal cost per unit of size, between the two largest sizes.
    marginal = "-"
    if members[2]["M instr"] is not None and members[1]["M instr"] is not None:
        marginal = f"{(members[2]['M instr'] - members[1]['M instr']) * 1e3 / (sizes[2] - sizes[1]):.0f}"
    print(f"{family:<16}{'/'.join(map(str, sizes)):>18}" + "".join(f"{cell:>12}" for cell in cells) + f"{marginal:>12}")
print(f"\nreports: {out}")
EOF
