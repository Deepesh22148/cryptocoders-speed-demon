# Track C: Join (Rust)

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/c-join.md).

## Approach

- **Language:** Rust, standard library only, one file: [src/main.rs](src/main.rs). The binary `track-c-rust` is copied to `/app/solution`.
- **I/O:** reads `/data/records.csv` and `/data/regions.csv`, writes `/out/aggregates.txt`. For local runs the paths can be overridden with `SPEED_DEMON_DATA_DIR` and `SPEED_DEMON_OUTPUT_DIR`.

The steps, in order:

1. **Regions.** `regions.csv` becomes a flat `u8` table indexed by `region_id` (1 MB). Countries are numbered in sorted order and `_UNMATCHED` takes the last number, so the output needs no sort.
2. **Split the file.** `records.csv` is cut into one byte range per hardware thread. A cut moves forward to the next LF followed by a digit or a BOM. Inside quotes an LF only occurs in notes, which are lowercase words (spec 2.2), so that pattern always marks a record start.
3. **Stream and parse.** Each thread streams its range through one fixed 64 KB buffer. The parser follows spec 2.1 byte for byte (BOM only at a record start, `""` escapes, CRLF or LF, an optional missing final terminator) and finds quotes and commas eight bytes at a time (SWAR). When the buffer ends mid-record, the tail moves to the front and the thread reads more.
4. **Aggregate.** Count, id sum, and amount sum/min/max in integer cents. Name and note byte counts are the raw length minus the number of `""` escapes. Each thread keeps its own 241 aggregates.
5. **Distinct names: one shared lock-free table.** The raw quoted bytes of a name map one to one to its unescaped value, so a 64-bit hash of them is the key. All threads share one open-addressing table of 131,072 slots, each slot holding `[hash, 256-bit group set]` (5.2 MB). A slot is claimed with a CAS and a group bit is set with `fetch_or`, after a plain load shows it is not set yet. The slot and the region entry are prefetched as soon as they are known, so their cache misses overlap the rest of the parse. The distinct count per group is a popcount over the table.

## Verification

- Full and sample outputs match the spec's seed-1 sha256 values (`d8088b1c...` and `9035fed3...`).
- Tiny matches the reference `c_join.py` output.
- The spec's worked example (section 4) matches byte for byte, also with the final terminator removed.

## Dependencies

None.

## With another 24 hours

1. Profile on the board's 12-thread box. Locally the shared name table is the largest cost (cache misses), then reading.
2. Give each name a dense id to shrink the table from 5.2 MB to about 4 MB, so it fits in cache better.
3. Known ceiling: names are compared by a 64-bit hash, not by their bytes. With the spec's 100,000-name pool the chance of a collision is about 1e-10.
