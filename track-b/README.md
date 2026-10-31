# Track B — Inverted Index (Rust)

Rust, standard library only (0 dependencies), in one file: [src/main.rs](src/main.rs). The build is `cargo build --release --offline`, and the binary is copied to `/app/solution`. It reads `/data` and writes `/out/results.txt`; for local runs, `SPEED_DEMON_DATA_DIR` and `SPEED_DEMON_OUTPUT_DIR` override the paths.

## Approach

The queries are read first, so the corpus scan only does work for the terms and phrases that are actually asked.

1. **Plan.** Every query term is interned into a map keyed by its bytes packed into a `u128` (terms of up to 16 bytes; longer ones use a byte-keyed map). Each term gets a BOOL posting id and/or a phrase term id. Each distinct phrase is keyed by its rarest adjacent term pair, measured on a 1 MB sample of the corpus, and a 1M-bit filter covers those pairs.
2. **Scan.** Each thread streams its own line-aligned byte range of `docs.txt`; nothing holds the whole file.
   - SSE2 turns every 64 bytes into bit masks of token bytes and newlines, and `trailing_zeros` walks the token boundaries.
   - A token's key is one 16-byte load, OR `0x20` (which lowercases alphanumerics), masked to its length.
   - At each newline, the doc's buffered term ids go through two tight loops:
     - BOOL terms are deduplicated and appended to a per-slice log. Terms already common in the slice's first 1024 docs set bitmap bits directly.
     - Adjacent phrase-id pairs are checked against the filter, and the few hits are verified against the full phrase.
3. **Index.** Each slice counting-sorts its log into CSR lists of `u16` slice-local doc indexes; terms in at least 1/32 of the slice's docs become bitmaps. Slices hold disjoint, ascending doc ranges, so they are never merged.
4. **Queries.** Threads take blocks of 1024 queries and walk the slices in order.
   - **BOOL** is evaluated left to right on each slice. The running result is a sorted index list while only sparse terms feed it, and switches to a bitmap when a common term is OR-ed in. Count and sum come from popcounts.
   - **PHRASE** results are already known from the scan.

## If I had another 24 hours

1. Profile on a 12-thread Linux box. Locally, the term lookup (about 11 ns per token) and the per-doc bookkeeping dominate.
2. Cut peak memory further: the per-slice log and CSR coexist while each slice is being built.
