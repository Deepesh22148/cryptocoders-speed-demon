# Track B — Inverted Index (Rust)

Rust, standard library only (0 dependencies), in one file: [src/main.rs](src/main.rs). The build is `cargo build --release --offline` for `x86-64-v3` (set in [.cargo/config.toml](.cargo/config.toml); the board's i5-13500 has AVX2, BMI2 and POPCNT), and the binary is copied to `/app/solution`. It reads `/data` and writes `/out/results.txt`; for local runs, `SPEED_DEMON_DATA_DIR` and `SPEED_DEMON_OUTPUT_DIR` override the paths.

## Approach

The queries are read first, so the corpus scan only does work for the terms and phrases that are actually asked.

1. **Plan.** Every query term is interned into a map keyed by its bytes packed into a `u128` (terms of up to 16 bytes; longer ones use a byte-keyed map). Each term gets a BOOL posting id and/or a phrase term id. A 256 KB sample of the corpus then measures the terms:
   - BOOL ids are renumbered by document frequency. Terms in at least 1/16 of the sample's docs come first and are bitmaps from the start.
   - Each distinct phrase is keyed by its rarest adjacent term pair. A 1M-bit filter covers those pairs, and a bit set marks the rarer term of each.
2. **Scan.** Each thread streams its own line-aligned byte range of `docs.txt`; nothing holds the whole file.
   - SSE2 turns every 64 bytes into bit masks of token bytes and newlines, and `trailing_zeros` walks the token boundaries.
   - A token's key is one 16-byte load, OR `0x20` (which lowercases alphanumerics), masked to its length.
   - At each newline, the doc's buffered term ids go through two tight loops:
     - Common BOOL terms set their bitmap bit directly. The rest are deduplicated and appended to a per-slice log.
     - At each token in the rarer-term set, the pairs around it are checked against the filter, and the few hits are verified against the full phrase.
3. **Index.** Each slice counting-sorts its log into CSR lists of `u16` slice-local doc indexes; terms in at least 1/32 of the slice's docs become bitmaps. A term's entry in a slice is one `[start, end]` or `[bitmap, NO]` pair. Slices hold disjoint, ascending doc ranges, so they are never merged.
4. **Queries.** Threads take blocks of queries (four per thread) and walk the slices in order, prefetching the entries and lists of the queries a few steps ahead.
   - **BOOL** is evaluated left to right on each slice. The running result is a sorted index list while only sparse terms feed it, and switches to a bitmap when a common term is OR-ed in. Count and sum come from popcounts.
   - **PHRASE** results are already known from the scan.

## If I had another 24 hours

1. Profile on the 12-thread board box. Locally, term lookups (about 15 ns per token, mostly hashbrown probes) and the per-doc bookkeeping dominate the scan.
2. Cut peak memory further. Each slice's log and CSR coexist while it builds; removing that overlap needs an in-place transpose.
