#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
target_dir="${CARGO_TARGET_DIR:-${repo_root}/target}/persistent-edge-codegen"
codegen_rustflags="${RUSTFLAGS:-}"
if [[ -n "${codegen_rustflags}" ]]; then
    codegen_rustflags+=" "
fi
codegen_rustflags+="-Aunused-variables"

cd "${repo_root}"

RUSTFLAGS="${codegen_rustflags}" CARGO_TARGET_DIR="${target_dir}" \
    cargo rustc --release -q -p glam-gc --features deterministic-test-hooks \
    --example persistent_edge_codegen -- --emit=llvm-ir

ir_file="$(ls -t "${target_dir}"/release/examples/persistent_edge_codegen-*.ll | head -n 1)"

extract_body() {
    local symbol="$1"
    sed -n "/^define .*@${symbol}(/,/^}/p" "${ir_file}"
}

duplicate_body="$(extract_body glam_gc_codegen_duplicate_u64)"
identity_body="$(extract_body glam_gc_codegen_same_allocation_u64)"

if [[ -z "${duplicate_body}" || -z "${identity_body}" ]]; then
    echo "persistent-edge code-generation symbols were not emitted" >&2
    exit 1
fi

forbidden='(^|[[:space:]])(alloca|atomicrmw|call|cmpxchg|fence|invoke)[[:space:]]'
if grep -Eq "${forbidden}" <<<"${duplicate_body}${identity_body}"; then
    echo "persistent-edge release code generation gained a call, allocation, or synchronization operation" >&2
    exit 1
fi

if [[ "$(grep -c '^  ' <<<"${duplicate_body}")" -ne 2 ]] \
    || [[ "$(grep -c ' = load ptr,' <<<"${duplicate_body}")" -ne 1 ]] \
    || [[ "$(grep -c '^  ret ptr ' <<<"${duplicate_body}")" -ne 1 ]]; then
    echo "Gc::duplicate_in no longer lowers to exactly one pointer load and return" >&2
    printf '%s\n' "${duplicate_body}" >&2
    exit 1
fi

if [[ "$(grep -c '^  ' <<<"${identity_body}")" -ne 4 ]] \
    || [[ "$(grep -c ' = load ptr,' <<<"${identity_body}")" -ne 2 ]] \
    || [[ "$(grep -c ' = icmp eq ptr ' <<<"${identity_body}")" -ne 1 ]] \
    || [[ "$(grep -c '^  ret i1 ' <<<"${identity_body}")" -ne 1 ]]; then
    echo "Gc::same_allocation_in no longer lowers to two pointer loads, comparison, and return" >&2
    printf '%s\n' "${identity_body}" >&2
    exit 1
fi

echo "persistent-edge release code generation is one pointer copy/comparison with no calls or synchronization"
