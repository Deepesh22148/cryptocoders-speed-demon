# Track B — Inverted Index (Rust)

## Approach

- **Language:** Rust, standard library only (0 dependencies).
- **Build:** `cargo build --release --offline`. The binary is `track-b-rust` and is copied to `/app/solution`.
- **I/O:** reads `/data/queries.txt` and `/data/docs.txt`, and writes `/out/results.txt`. You can override the paths with `SPEED_DEMON_DATA_DIR` and `SPEED_DEMON_OUTPUT_DIR` for local runs.

The steps, in order:

1. **Parse queries first.** Each distinct term gets a `u32` id, and only those terms are indexed ([parser.rs](src/parser.rs)).
2. **Index in parallel** ([index.rs](src/index.rs)).
   - The corpus is cut into one line-aligned slice per hardware thread.
   - Each thread tokenizes its slice with a 256-entry lookup table and finds each token's id in an Fx-hashed map.
   - Each slice records two things:
     - sorted doc ids for each term
     - each document's token-id stream
3. **Slices are never merged.** Each slice covers a disjoint, ascending range of doc ids. So a query runs on every slice, and the slice results are joined in order: counts and sums add up, and the first 10 ids come from the earliest slices.
4. **Answer queries in parallel** ([query.rs](src/query.rs)). Threads pull blocks of 64 queries from an atomic counter.
   - **BOOL:** strictly left to right. The first list is borrowed, not copied. Intersect and difference switch to binary search when one list is 8 or more times smaller than the other.
   - **PHRASE:** intersect the term doc lists, rarest first. In each candidate doc's id stream, find the rarest term and compare the whole window around it.

## Verification

- Checked against the reference `b_index.py` on synthetic data built from the spec: identical output.
- The output is also identical to the previous Rust version, whose real-data sha256 matched the reference (`F86973C2...`).
- Details are in [PERF.md](PERF.md).

## If I had another 24 hours

1. Push, record the board run IDs, and check the real full dataset against its published hash.
2. Profile on Linux with 12 threads, and check where index and query time go there.
3. If phrases with no anchor show up as hot, add positions for the few very common terms only.
