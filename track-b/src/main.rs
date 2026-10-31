mod index;
mod parser;
mod query;

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

const BLOCK: usize = 64;

fn dir(var: &str, default: &str) -> PathBuf {
    std::env::var_os(var).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(default))
}

fn main() {
    // /data and /out in the board; overridable for local runs.
    let data = dir("SPEED_DEMON_DATA_DIR", "/data");
    let out_dir = dir("SPEED_DEMON_OUTPUT_DIR", "/out");
    let threads = thread::available_parallelism().map_or(12, |n| n.get());

    let qtext = std::fs::read(data.join("queries.txt")).expect("read queries.txt");
    let docs = std::fs::read(data.join("docs.txt")).expect("read docs.txt");
    let parsed = parser::parse(&qtext);
    let ix = index::build(&docs, &parsed.terms, threads);
    drop(docs);

    // Answer blocks of queries in parallel, then stitch them back in order.
    let qs = &parsed.queries;
    let blocks = qs.len().div_ceil(BLOCK);
    let next = AtomicUsize::new(0);
    let mut done: Vec<(usize, Vec<u8>)> = thread::scope(|s| {
        let hs: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let b = next.fetch_add(1, Ordering::Relaxed);
                        if b >= blocks {
                            return mine;
                        }
                        let mut buf = Vec::with_capacity(BLOCK * 40);
                        for q in &qs[b * BLOCK..((b + 1) * BLOCK).min(qs.len())] {
                            query::answer(&ix, q, &mut buf);
                        }
                        mine.push((b, buf));
                    }
                })
            })
            .collect();
        hs.into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    done.sort_unstable_by_key(|d| d.0);

    std::fs::create_dir_all(&out_dir).expect("create output dir");
    std::fs::write(out_dir.join("results.txt"), done.into_iter().flat_map(|d| d.1).collect::<Vec<u8>>())
        .expect("write results.txt");
    // Skip freeing the index; the OS reclaims it.
    std::process::exit(0);
}
