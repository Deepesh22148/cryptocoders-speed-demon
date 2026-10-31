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
- **Phrases.** Keeping positions cost about 1 s of indexing, and re-scanning document text cost 10.9 GB of re-lexing on phrases made only of common words. Matching phrases during the scan removes both. Each phrase is keyed by its rarest adjacent pair, measured on a 1 MB sample, behind a 1M-bit filter, and verified on the doc's term-id buffer.
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
