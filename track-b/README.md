# Track B — Inverted Index (Rust)

## Approach

- **Language:** Rust, standard library only (0 dependencies), in one file: [src/main.rs](src/main.rs).
- **Build:** `cargo build --release --offline`. The binary `track-b-rust` is copied to `/app/solution`.
- **I/O:** reads `/data/queries.txt` and `/data/docs.txt`, and writes `/out/results.txt`. For local runs, the paths can be overridden with `SPEED_DEMON_DATA_DIR` and `SPEED_DEMON_OUTPUT_DIR`.

The steps, in order:

1. **Parse the queries first.**
   - Every distinct term gets a posting id.
   - Terms that appear in phrases also get a dense `u16` phrase id. The spec caps phrase-term uses at 19,000 × 8, so `u16` is enough.
   - Only these terms are indexed.
2. **Stream and index in parallel.** Each hardware thread reads its own byte range of `docs.txt` in 1 MB chunks, so the corpus is never held whole. For its range, a thread records:
   - sorted doc ids for each query term
   - each document's token stream as phrase ids
3. **Never merge the slices.** Each slice covers a disjoint, ascending range of doc ids. A query runs on every slice, and the results are joined in order: counts and sums are added, and the first 10 ids come from the earliest slices.
4. **Answer queries in parallel.** Threads take blocks of 64 queries from an atomic counter.
   - **BOOL** runs strictly left to right. AND and NOT share one filter that switches to binary search when the other list is 8 or more times larger.
   - **PHRASE** intersects the doc lists of its terms. In each candidate doc's token stream, it finds the rarest term and compares the window around it.

## Verification

- The output is byte-identical to the reference `b_index.py` on synthetic data built from the spec.
- It also matches the earlier Rust version, whose sha256 on the real data matched the reference (`F86973C2...`).

## If I had another 24 hours

1. Compress postings (delta + varint) to reduce peak memory further.
2. Profile on the board's 12-thread Linux box to see whether indexing or queries dominate there.
