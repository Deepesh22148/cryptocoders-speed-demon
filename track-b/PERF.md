# Track B — Performance Log

This log distinguishes local measurements from official live-board measurements. Add the official run ID for every experiment after pushing it to the Speed Demon board.

## Baseline

- Implementation: initial Rust Track B implementation.
- Local full dataset: 250,000 documents and 50,000 queries.
- Previously recorded local end-to-end runtime: approximately **60.57 s**.
- Correctness: the baseline output SHA-256 matched the Python reference.

## Optimized implementation

Changes include:
- Adaptive intersection and difference strategies for skewed posting-list sizes.
- Rarest-term anchoring for phrase-query candidate documents.
- Stack-backed storage for phrase position slices during candidate evaluation.

Previously recorded local full-dataset runtime: approximately **14.18 s**.

- Approximate local speedup versus the earlier Rust baseline: **4.27×**.
- Full-output SHA-256: `F86973C210228141BCB6F1F1B10188AD2A9B65F71E7CD7CF4590B38DE4E80927`
- Python reference SHA-256: `F86973C210228141BCB6F1F1B10188AD2A9B65F71E7CD7CF4590B38DE4E80927`
- Correctness: byte-for-byte output hash match confirmed locally.

## Official benchmark runs

| Version / change | Local runtime | Official run ID | Official result |
|---|---:|---|---|
| Initial Rust implementation | ~60.57 s | Pending | Pending |
| Adaptive posting-list operations + rarest-anchor phrase search | ~14.18 s | Pending | Pending |

## Notes and limitations

- The runtimes above are local measurements, not competition scoring measurements.
- No official live-board run IDs are available yet; do not invent or backfill run IDs.
- The Docker execution contract and official harness still need to be tested.
- Record unsuccessful experiments here too, including the reason each was reverted.
