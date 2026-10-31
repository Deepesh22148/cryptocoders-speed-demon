# Track C: performance log

Every step from the reference to your final version, each with the live-board run ID that measured it. Judges check that the run IDs exist and that each change moved the way you say it did.

Local times are from a Windows laptop (8 hardware threads) on the seed-1 full dataset, so they only compare one version with another.

| Step | Change | Run ID | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 14.44 | 367 | 0 |
| 1 | Rust, std only: byte-level parser per spec 2.1, file split at record starts across all threads, 1 MB streamed reads per thread, regions as a flat `u16` table, distinct names as per-group bitsets over interned ids | Pending | local 0.66 | | |
