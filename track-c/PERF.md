# Track C: performance log

Every step from the reference to your final version, each with the live-board run ID that measured it. Judges check that the run IDs exist and that each change moved the way you say it did.

Local times are from a Windows laptop (8 hardware threads) on the seed-1 full dataset, so they only compare one version with another.

| Step | Change | Run ID | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 14.44 | 367 | 0 |
| 1 | Rust, std only: byte-level parser per spec 2.1, file split at record starts across all threads, 1 MB streamed reads per thread, regions as a flat `u16` table, distinct names as per-group bitsets over interned ids | Pending | local 0.66 | local 113 | |
| 2 | Per-thread name hash maps and bitsets (113 MB) replaced by one shared lock-free table: `[hash, 256-bit group set]` per slot, set with CAS and `fetch_or`. Fixed 64 KB read buffer, `u8` region table | Pending | local 0.23 | local 17 to 21 | |
| 3 | SWAR scan for `"` and `,`; names hashed as whole words; notes no longer hashed | Pending | local ~0.22 (single-thread 0.67 to 0.62) | | |
| 4 | Prefetch the name-table slot right after the name is parsed, and the region entry right after `region_id`, so the cache misses overlap parsing | Pending | single-thread 0.67 to 0.53 | | |
| 5 | Source shrunk for the S axis (108 reference lines): about 257 to 213 lines after rustfmt, with no change in behaviour | Pending | same | local 16.6 | |

Local single-thread profile before step 4, found by switching parts off one at a time: name table ~260 ms, hashing ~26 ms, region lookup ~44 ms, read ~180 ms (Windows), parse ~100 ms. Windows process start-up alone is ~60 ms, so local wall times overstate the board's.

Tried and reverted: collecting a chunk's (hash, group) pairs and marking them in one batch (slower); 2^18 table slots (no gain, since the cost is cache misses, not probe length); 256 KB and 1 MB read buffers (no gain).

## Linux measurements

From step 6 on, numbers are from a static Linux (musl) build run in WSL2 on the same laptop (8 threads), with the data in tmpfs, measured with `busybox time` (min of 10 interleaved runs, max RSS). The board uses a glibc build with 12 threads, so its numbers will differ a little.

| Step | Change | Run ID | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 5 | Previous push, measured on Linux | | 0.18 | 20.2 | |
| 6 | regions.csv text and parsed rows (2.2 + 6.4 MB) freed before the scan; they were alive for the whole run and set the peak | Pending | 0.15 | 8.5 | |
| 7 | Region loading rewritten: country codes as base-26 indexes instead of a BTreeSet of 200k strings and string binary searches. In-program timer: 40–57 ms down to ~9 ms | Pending | | | |
| 8 | Distinct names counted when `fetch_or` first sets a bit, so the popcount pass over the table (~7 ms) is gone | Pending | | | |
| 9 | SWAR digit parse: up to seven digits per 64-bit word, for id, region_id and amount | Pending | 0.12 (steps 7–9 together) | 6.8 | |
| 10 | Read buffer 64 KB → 32 KB, for memory headroom on the board's 12 threads | Pending | 0.11–0.12 | 6.8 | |

After step 9, peak RSS is under 7.3 MB, which is M_ref / 50, so the memory factor is at its cap.

Linux profile before step 7 (in-program timers): regions ~45 ms, scan ~130 ms, merge ~7 ms. Raw read of the file from tmpfs is ~0.02 s single-threaded, so the scan is compute-bound.
| 11 | Name-table bit set with a plain `fetch_or` (no load-then-check: ~97% of marks set a new bit, so the check only added a shared-to-exclusive upgrade), and the slot prefetched with `prefetchw` (inline asm, so no unstable target feature) so it arrives in exclusive state for the atomic | Pending | min −4% (`fetch_or`), −5 to −11% with `prefetchw` | 6.8 | |

Linux ablation before step 11 (min of 15 interleaved runs, 8 threads): total ~100–112 ms, name table ~37 ms, hashing ~9 ms, parsing ~39 ms, read + line scan ~27 ms (includes ~6 ms region load and ~10 ms start-up).

Tried and dropped in this round: an 8- or 16-deep pipeline that delays each table update so its prefetch has more lead time (≤3%, within noise, adds code); a single-pass region load through a temporary `u16` table (6 ms → 9 ms, more page faults).

Measurement note: during this round a long-running background job held one to two cores, so wall-time medians were noisy; decisions used min times of interleaved A/B runs.

## Board check and parser rewrite

Board after step 11: **0.052 s, 10 MB, 242 lines, K 30.53** (rank 13; leader 0.018 s). The harness reads memory from the run cgroup's `memory.peak` after pre-loading `/data` into the page cache, so the ~10 MB is mostly container baseline.

| Step | Change | Run ID | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 12 | Parser rewritten as a 64-byte block scanner: SSE2 masks of `"`, `,` and LF, a prefix XOR of the quote mask marks bytes inside quotes (`""` escapes cancel out), and commas and LFs outside quotes give every field boundary. Unescaped lengths come from popcounts of the quote mask between boundaries. Name hash reads the first and last eight bytes instead of looping with a padded tail. Table updates go through a 16-record queue so each slot prefetch lands before its update. Still streamed through a 64 KB buffer per thread | Pending | local: 1 thread 0.36 → 0.34; 8 threads ~5% slower than step 11 | | |

Pushed as a board experiment: on this 4-core laptop with an 8 MB L3 the 8-thread run is bound by name-table cache misses, which hides parser gains, while the board has six faster cores and a larger cache. Revert to step 11 if the board time is not at least ~6% better (the source grew from 235 to 283 lines).

Local single-thread profile of step 12: reading and boundary detection 0.13 s (including start-up), record arithmetic 0.07 s, name hash 0.05 s → ~0.03 s after the new hash, name-table updates 0.09 s.

Tried and dropped this round:
- `mmap` of the whole file (free on the board's memory measure): equal single-threaded, 10–20% slower at 8 threads in WSL2, where page faults are expensive; `MAP_POPULATE` and per-thread `MADV_POPULATE_READ` did not help.
- Name table split by hash into per-thread shares, fed through lock-free single-producer queues so updates need no locked instruction and stay in L2: correct under stress, but 124 ms against 99 ms at 8 threads (cross-core queue traffic costs more than the misses it saves here), and very slow with 32 threads and 2-entry queues.
- Removing the lock from the table update (single-thread test): 0.37 → 0.35 s, so the cost is cache misses, not the `lock` prefix.
- Separate key array for the table (shorter probes): no change.
