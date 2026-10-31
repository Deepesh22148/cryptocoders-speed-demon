# Track B — Inverted Index (Rust)

## Approach

This submission builds an inverted index for the Track B document corpus and evaluates Boolean and phrase queries.

- **Language:** Rust
- **Runtime dependencies:** None declared (standard library only; verify `Cargo.toml` before submission).
- **Build:** `cargo build --release --offline`
- **Executable:** `track-b-rust`

The implementation uses sorted document postings for Boolean operations. Phrase queries use token positions and a rare-term candidate anchor to reduce candidate checks.

## Execution contract

The competition runner mounts the input dataset read-only at `/data` and the output directory at `/out`. The program must read the Track B input files from `/data` and write exactly `/out/results.txt`, then exit successfully.

**Important:** The current local development version used relative paths (`../data/b_full`). Before submitting, update `src/main.rs` to use `/data/docs.txt`, `/data/queries.txt`, and `/out/results.txt`, and verify it with the official harness. The Dockerfile does not rewrite those paths.

## Build

The Dockerfile uses the competition-published, digest-pinned Rust and Debian images and builds with Cargo offline. It expects the Cargo package/binary name to be `track-b-rust`. If your `[package].name` in `Cargo.toml` differs, change the binary path in the Dockerfile accordingly.

## Verification

The local full-dataset output was compared with the Python reference and the SHA-256 hashes matched:

`F86973C210228141BCB6F1F1B10188AD2A9B65F71E7CD7CF4590B38DE4E80927`

This local result does not replace a successful official Docker/harness run.

## If I had another 24 hours

1. Run the official harness repeatedly and retain live-board run IDs.
2. Profile the scoring-machine workload before making further algorithm changes.
3. Re-check peak memory, startup overhead, and deterministic output under the read-only sandbox.
