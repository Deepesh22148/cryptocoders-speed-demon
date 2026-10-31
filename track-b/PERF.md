# Track B — Performance Log

Local numbers are from a Windows laptop (8 hardware threads). The board has 12 threads and runs Linux, so these numbers are only for comparing one version with another. Add the official run ID for every step once it is pushed.

## Test data

The generator in the organisers' repo was not run locally. Instead, a synthetic generator built from `specs/b-index.md` produced 250,000 docs (255 MB) and 50,000 queries. It follows the spec's Zipf vocabulary, document-length buckets, separators, non-ASCII words, caps and repeated runs. Query terms follow the spec's df bands. Its BOOL answer counts are median 1, p90 1.2k and p99 59k; the spec reports 8, 2.6k and 80k.

How correctness was checked:

- On a 20k-doc set, the output is byte-identical to the reference `b_index.py`.
- On every full-size set, the output is identical to the previous Rust version, which had already matched the reference sha256 on the real data (`F86973C2...`).

## Steps

| # | Change | Local wall time | Official run ID |
|---|---|---:|---|
| 0 | Initial Rust | ~60.6 s (real data) | — |
| 1 | Adaptive intersect/difference, rarest-anchor phrases | ~14.2 s (real data); 42 s on synthetic | Pending |
| 2 | Whole-file reads, Fx hash, query terms interned to ids, parallel indexing over line-aligned slices, parallel query blocks, LTO | 7.5 s | Pending |
| 3 | Slices are never merged: each owns a disjoint ascending doc range, so every answer is the concatenation of per-slice answers (the merge cost ~1 s) | 2.5 s | Pending |
| 4 | Positional postings removed: each doc stores its token-id stream (sequential pushes), and phrases intersect doc lists then match the id window around the rarest term | 2.2 s | Pending |

Phase split after step 4: read ~0.15 s, index ~0.9 s, queries ~0.9 s.

## Tried and reverted

- **Re-scanning document text to check phrases.** Indexing got faster, but about 400 phrases made only of common words had huge candidate sets. That meant 10.9 GB of text re-lexed and the query phase took ~10 s. The token-id streams of step 4 replaced it.

## Known ceiling

A phrase with no rare term (for example `"the the"`) scans the id stream of every doc that holds all of its terms. Adding 192 such phrases cost about +1.3 s locally. The spec has about 92 phrases with no anchor. If they turn out to dominate, add positions for very common terms only.
