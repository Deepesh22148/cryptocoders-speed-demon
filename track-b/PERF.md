# Track B — Performance Log

Scoring is `K = (T_ref/T)^0.5 x min(M_ref/M, 50)^0.2 x min(S_ref/S, 1)^0.15`, with T_ref 56.35 s, M_ref 2.2 GB and S_ref 91 lines (from `harness/scoring.py` and `calibration-seed1.json`). Time therefore dominates: halving T multiplies K by 1.41, while halving M multiplies it by 1.15.

## Board measurements

The public board feed keeps only each team's best and latest run, so earlier run IDs were not retained; those rows list the commit that was measured instead.

| Commit | Change | T (s) | M (MB) | S | K | Points | Run |
|---|---|---:|---:|---:|---:|---:|---|
| `1214719` | First Rust version: postings plus positions, single-threaded | 0.821 | 585 | 383 | 8.70 | 47.0 | not retained |
| `dd4924d` | Corpus streamed per thread, phrases matched during the scan, BOOL postings in CSR from an append-only log | 0.733 | 221 | 335 | 11.31 | 52.7 | not retained |
| `72b30e2` | SSE2 tokenizer (64-byte token and newline bit masks), term keys from one 16-byte load | 0.412 | 209 | 412 | 14.79 | 58.5 | not retained |
| `615b382` | BOOL queries evaluated as slice-local bitmaps; common terms stored as bitmaps; early dense detection | 0.371 | 183 | 501 | 15.55 | 59.6 | not retained |
| `6848d3e` | u16 logs and doc indexes, flat phrase hits, phrase anchors picked from a corpus sample | 0.341 | 115 | 529 | 17.65 | 62.3 | not retained |
| `88ea70a` | Per-doc term loops split from the lexer; terms interned straight into the u128 map | 0.306 | 112 | 533 | 18.72 | 63.6 | `run-268e28ea65a5` |
| `5760c1a` | Two slices per thread, per-doc dedupe first, flat early bitmaps | 0.359 | 99 | 532 | 17.71 | 62.4 | `run-e025b3e88318` |

`5760c1a` was slower. Each query visits every slice, so doubling the slices doubled the scattered per-slice lookups in the query phase. `c1dc498` goes back to one slice per thread, and `e645618` answers queries in blocks of 1024, slice by slice, so each slice's index stays cache-hot.

## Local measurements since `e645618`

These were measured on the real seed-1 `full` data (`docs.txt` sha256 `6cd46167…`), with the output checked against the published `results.txt` hash (`f86973c2…`) and the `sample` hash (`eeeec1e9…`) after every step. The board rows follow once the commits are pushed.

| Commit | Change | Effect |
|---|---|---|
| `e6edf79` | Build for `x86-64-v3` (`.cargo/config.toml`) | Query phase -8%, index -3%. Without it `count_ones` is a bit-twiddling sequence, not `POPCNT`. The board box is an i5-13500, which has AVX2, BMI2 and POPCNT. The level is named explicitly, not `native`, so the Depot build is unaffected. |
| `e6edf79` | BOOL ids ordered by document frequency in the corpus sample; ids below `early` are bitmaps from the first doc | Removes the per-slice 1024-doc probe and its count pass. Early terms set their bit without the per-term dedupe, since setting a bit twice is harmless. Index about -3%. |
| `e6edf79` | Phrase pairs checked only around the rarer pid of an anchor pair | The old filter was the pair's first pid: 20.3M of 36M token pairs passed it, because common words start many anchors. |
| `e6edf79` | Sparse BOOL lists: first term copied, OR merges two sorted runs (`sort` instead of `sort_unstable`), AND searches the longer list for each id of the shorter | Query phase -23% (383 to 293 ms single-threaded). 233k of the 248k slice evaluations end as lists, and the first sparse term used to go through a copy, `sort_unstable` and `dedup`. |
| `e6edf79` | One `[start, end]` or `[bitmap, NO]` entry per term instead of `off` and `bm` | One load per term per slice. The build no longer shifts `off` back into place. |
| `e6edf79` | Query blocks prefetch term entries 8 queries ahead, and their lists or bitmaps 4 ahead | Query phase -5 to -8%. Even queries with an empty answer cost 0.7 µs per slice, because 12 slices of about 3 MB each do not fit in L3. |
| `35de5ea` | A BOOL query is one `Vec<(op, term)>`; phrases deduped by line text before parsing; 256 KB corpus sample instead of 1 MB | Parse 28 to 26 ms, then to about 15 ms with the smaller sample. Parse runs on one thread, so it is wall time. |
| `35de5ea` | Query blocks sized to four per thread | 12 threads used to get 49 blocks of 1024, and the 49th ran alone. Now they get 48 blocks of 1042. |

Overall, the whole run on a 4-core laptop went from 839 to 728 ms (min of 11, interleaved) after `e6edf79`. In Linux (Docker, 8 threads), it went from 659 to 581 ms (min of 6) after `35de5ea`. Peak memory is unchanged at 75-95 MB.

## Memory

Measured as `VmHWM` with 12 threads in Linux (Docker), which is steadier than the cgroup number. Before these changes, the peak was 92 MB:
- 16 MB of parse state
- 45 MB of final index: ids 21 MB, bitmaps 11 MB, term entries 5 MB
- a 31 MB build transient: logs 26 MB, early bitmaps about 8 MB, dedupe state 2.5 MB

Every slice builds at about the same moment, so the build is the peak.

| Change | Peak RSS |
|---|---:|
| Before | 92 MB |
| Each slice frees its dedupe state, and its early bitmaps once they are copied, before allocating its ids | 67-75 MB |
| Query text and corpus sample freed after parsing (the one term over 16 bytes now owns its key); term-table keys as two `u64`s, so buckets are 24 bytes instead of 32 | 63-69 MB |

The range comes from how many slices happen to build at once. Time is unchanged (interleaved A/B, medians equal).

Measured and not kept:
- An early-bitmap threshold of 1/32 was worse (73-87 MB).
- A final bitmap threshold of 1/16 saved about 1 MB.
- Gap-encoded ids would be only 23% smaller (varint), since most gaps are 128 or more, and every list would then need decoding at query time.

## How correctness is checked

The organisers' generator was not run locally. Instead, a generator written from `specs/b-index.md` produces 250,000 docs (255 MB) and 50,000 queries. It follows:
- the Zipf vocabulary and document-length buckets
- the separators, non-ASCII words, upper/title case and repeated runs
- the df bands for query terms

Every change is checked on four datasets:
- a 20k-doc set, against the reference `b_index.py`
- a full-size set whose phrases are mostly common words, against `b_index.py` (byte-identical)
- a spec-shaped full-size set
- the same set plus 192 phrases made only of common words

The board's own correctness gate (sha256 on the real data) has passed on every run.

## What the profile showed

Local timings come from a 4-core laptop, and they swing about 40% between identical runs, so changes were compared with interleaved A/B runs (min of 9) or measured on the board.

- **Lexing.** A byte loop took 2.6 ns/byte single-threaded. SSE2 masks walked with `trailing_zeros` take 0.45 ns/byte.
- **Term lookup.** It looked expensive (37 ns per token). The cause was a variable-length `copy_from_slice` that compiled to a `memcpy` call per token. A fixed 16-byte load plus a mask brought it to about 11 ns, close to the cost of a hash probe.
- **Hasher.** The first Fx-style hasher returned `word * K`. Its low bits, which pick the hashbrown bucket, depended only on the first letters. A folded 128-bit multiply fixes that.
- **Phrases.** Keeping positions cost about 1 s of indexing, and re-scanning document text cost 10.9 GB of re-lexing on phrases made only of common words. Matching phrases during the scan removes both. Each phrase is keyed by its rarest adjacent pair, measured on a corpus sample (now 256 KB), behind a 1M-bit filter, and verified on the doc's term-id buffer.
- **Postings.** Growing one vector per term meant about 2.4M reallocations. An append-only log per slice, counting-sorted once into CSR, removed them. Terms that are common in a slice become bitmaps.
- **Memory** (cgroup `memory.peak`; `/data` is pre-warmed outside the cgroup, so reading the corpus is free). The changes were:
  - streaming instead of holding the 255 MB file
  - u16 log entries and doc indexes
  - flat phrase hits
  - reserving log capacity, since untouched pages are not charged

## Tried and reverted

- **Positional postings for phrase terms.** Correct, but pushing positions for common words cost about 1 s of indexing locally.
- **Re-scanning candidate documents' text for phrases.** Phrases with no rare word produced millions of candidates.
- **A direct-mapped cache in front of the term table, a custom linear-probing table, and separate maps for keys of up to 8 bytes.** All were as slow as hashbrown or slower in an in-memory microbenchmark.
- **Batching term lookups for memory-level parallelism.** Only 5% faster in the microbenchmark, so not worth the code.
- **Two slices per thread.** See above.
- **24-byte term-table buckets** (key as two `u64`s instead of a 16-aligned `u128`, so 3 MB instead of 4 MB). No measurable change.
- **Early bitmaps laid out block-major** (each 64-doc block's words for all early terms together, transposed at build). It was faster single-threaded and slower with 8 threads, and peak memory did not move, so it was not worth the transpose.
- **`target-cpu=alderlake` instead of `x86-64-v3`.** Same speed.
- **A 1/32 early-bitmap threshold instead of 1/16.** Peak memory did not move.
- **Evaluating BOOL queries during the scan, with no postings at all.** This was not built. The 31,000 BOOL answers hold 88M documents in total, so the per-document evaluations would cost more time than the memory they save is worth.

## Where the time goes now

Single-threaded on the laptop, a 32 MB slice takes about 185 ms:
- lexing and reading, 43 ms
- term lookups, 68 ms (about 15 ns per token, mostly hashbrown probes)
- per-doc BOOL bookkeeping, 48 ms
- the phrase loop, 21 ms

In wall time with 8 threads, parse is about 5%, the scan and build about 75%, and queries about 15%.

What is left of the peak is mostly the build: each slice holds its term log (2.2 MB) and its CSR ids (1.8 MB) at the same time. Removing that overlap needs an in-place transpose, which was estimated at 60 ms of extra build per slice.
