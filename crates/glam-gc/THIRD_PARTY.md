# Third-Party Source Provenance

No source code has been copied or adapted into `glam-gc` as of Phase C0.

The crate has no normal dependencies. Loom is used unmodified as a development
dependency for modeled concurrency tests and is recorded by `Cargo.lock`; it is
not incorporated source.

Before copying or adapting collector code, add an entry containing:

- upstream project and repository URL;
- exact release and revision;
- upstream source paths and corresponding local paths;
- whether each local file was copied or adapted;
- upstream copyright notices;
- SPDX license expression and a verbatim license file below `LICENSES/`; and
- compatibility/review notes.

Candidate implementations named in the implementation plan—Sandpit, Abfall,
gc-arena, and Rudo—are research references only. Naming one in a plan does not
mean its code has been incorporated.
