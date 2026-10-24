# Performance record

Measured numbers over time. Every entry names the commit, the host and the
cargo profile it was taken under; numbers taken under different profiles do
not compare, and neither do `brokkr dellingr` rows stored on either side of a
profile change (`.brokkr/results.db` keeps them, filed under the same
workload names).

## Build profiles

Until 0779c08 the repository's `[profile.release]` set fat LTO and one
codegen unit, and every release benchmark, envelope figure and README timing
taken before then is under that profile. Since the change after 0779c08 the
repository sets no release overrides, so release means Cargo's default
release profile (opt-level 3, thin-local LTO, 16 codegen units), the profile
an embedder most likely builds dellingr under. The native-stack figures for
both profiles are in reference/native-calls.md ("Cargo's default release
profile").

## Release profile change, 2026-10-25

Host plantasjen (AMD Ryzen 9 5900X, x86-64 Linux 7.3, governor
`performance`), `rustc 1.101.0-nightly (e9bbadf68 2026-10-12)`, host
rustflags `-Ctarget-cpu=native` with the `wild` linker. The same source,
0779c08, built twice by `brokkr dellingr --bench` (best of 3 launches per
cell, 20 warm calls each): **A** under the old fat-LTO, one-codegen-unit
profile (`--commit 0779c08`, stored rows), **B** under Cargo's default
release profile (the working tree with only the profile removed, run with
`--force` because the tree was dirty, so not stored). Three interleaved
rounds, A B, B A, A B. Figures are the harness's `warm_avg_us`, in ms.

| Workload | A rounds | B rounds | A median | B median | B vs A |
|---|---|---|---|---|---|
| `benchmark` | 103.00, 101.72, 102.01 | 105.19, 105.22, 105.92 | 102.01 | 105.22 | +3.1% |
| `arithmetic` | 67.77, 67.80, 67.58 | 67.48, 67.25, 67.56 | 67.77 | 67.48 | -0.4% |
| `method` | 77.55, 73.58, 74.05 | 74.53, 73.83, 74.48 | 74.05 | 74.48 | +0.6% |
| `mixed` | 71.87, 72.54, 71.52 | 74.68, 77.72, 78.23 | 71.87 | 77.72 | +8.1% |
| `gc_churn` | 78.14, 76.99, 77.35 | 86.69, 86.92, 87.69 | 77.35 | 86.92 | +12.4% |
| `sort` | 100.72, 102.60, 102.64 | 101.33, 101.73, 101.80 | 102.60 | 101.73 | -0.8% |

Every pair of `benchmark`, `mixed` and `gc_churn` is slower under the default
profile (by 2.1 to 3.8%, 3.9 to 9.4% and 10.9 to 13.4% per round), so those
three are real costs of the profile; which functions lost what was not
attributed. `arithmetic`, `method`
and `sort` change by less than their round-to-round spread, with mixed
signs, so they show no effect. No same-binary control was run for these
cells; binary layout differences between the two builds are part of what
this measures.
