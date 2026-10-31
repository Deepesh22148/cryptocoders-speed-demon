# Track C: Join (Rust)

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/c-join.md).

## Approach

- **Language:** Rust, standard library only, one file: [src/main.rs](src/main.rs). The binary `track-c-rust` is copied to `/app/solution`.
- **I/O:** reads `/data/records.csv` and `/data/regions.csv`, writes `/out/aggregates.txt`. For local runs the paths can be overridden with `SPEED_DEMON_DATA_DIR` and `SPEED_DEMON_OUTPUT_DIR`.

The steps, in order:

1. **Regions.** `regions.csv` becomes a flat `Vec<u16>` indexed by `region_id`. Countries are numbered in sorted order and `_UNMATCHED` takes the last number, so the output needs no sort.
2. **Split the file.** `records.csv` is cut into one byte range per hardware thread. A cut moves forward to the next LF followed by a digit or a BOM. Inside quotes an LF only occurs in notes, which are lowercase words (spec 2.2), so that pattern is always a record start.
3. **Stream and parse.** Each thread reads its range in 1 MB chunks and runs a byte-level parser that follows spec 2.1: BOM only at a record start, quoted fields end at a `"` that is not part of `""`, CRLF or LF terminators, an optional missing terminator at end of file. When a chunk ends mid-record the parser stops and the thread reads more.
4. **Aggregate.** Count, id sum, amount sum/min/max in integer cents, and name/note byte lengths (raw length minus the number of `""` escapes) per group. Distinct names: the raw quoted bytes map one to one to the unescaped value, so each thread interns them to ids and sets a bit per group.
5. **Merge.** Thread name ids are mapped to global ids, the per-group bitsets are OR'ed, and the distinct count is a popcount.

## Verification

- Full and sample outputs match the spec's seed-1 sha256 values (`d8088b1c...` and `9035fed3...`).
- Tiny matches the reference `c_join.py` output.
- The spec's worked example (section 4) matches byte for byte, also with the final terminator removed.

## Dependencies

None.

## With another 24 hours

1. Use `memchr`-style SIMD scanning for quotes and LFs inside long notes.
2. Profile on the board's 12-thread box to see whether reading or hashing names dominates.
