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
