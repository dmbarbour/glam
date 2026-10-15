//! Loops over mutator entries, suspensions or allocator acquisitions, for
//! instruction counts.
//!
//! `cargo run --release --example entry_cost -- <mode> <count>`, where mode
//! is `mutate` (an outer entry and exit), `suspend` (a suspension and
//! resumption inside one entry), `allocator` (acquiring and dropping an
//! allocator for a cached class inside one entry) or `idle` (none, the
//! baseline). Compare
//! `perf stat -e instructions:u` across two counts: the difference over the
//! difference in count is one operation's cost.

use std::hint::black_box;

use glam_gc::Heap;

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let mode = arguments.first().map_or("mutate", String::as_str);
    let count: u64 = arguments
        .get(1)
        .map_or(1_000_000, |count| count.parse().expect("count is a number"));
    let heap = Heap::new();
    // One entry first, so every measured one finds the thread's record
    // and every measured acquisition finds its class cached.
    heap.mutate(|mutator| {
        drop(black_box(
            mutator.allocator::<u64>().expect("u64 has a class"),
        ));
    });
    match mode {
        "mutate" => {
            for _ in 0..count {
                heap.mutate(|mutator| {
                    black_box(mutator);
                });
            }
        }
        "suspend" => heap.mutate(|mutator| {
            for _ in 0..count {
                mutator.suspend(|| black_box(()));
            }
        }),
        "allocator" => heap.mutate(|mutator| {
            for _ in 0..count {
                drop(black_box(
                    mutator.allocator::<u64>().expect("u64 has a class"),
                ));
            }
        }),
        "idle" => {
            for index in 0..count {
                black_box(index);
            }
        }
        other => panic!("unknown mode {other}"),
    }
}
