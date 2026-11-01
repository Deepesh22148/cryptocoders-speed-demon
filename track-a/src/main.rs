//! Track A (Ingest): per (hour, endpoint) count, nearest-rank p50/p95/p99, 5xx errors.
//!
//! Phase 1 (12 threads, dynamic 1 MB chunks of an mmap'd file): one forward pass per line,
//!   fixed-offset hour, SWAR space scan fused with endpoint hashing, SWAR latency parse, one
//!   u64 record pushed into a pre-sized per-thread, per-hour bucket. No line splitting, no
//!   copies, no date parsing.
//! Merge (tiny): endpoint dictionaries are unified and sorted bytewise once.
//! Phase 2 (12 threads, dynamic, one hour per work item): counting-sort that hour's records
//!   by endpoint rank in an L2-resident scratch array, select the percentiles, format the
//!   text. Iterating ranks in order yields the required output order.
//! Phase 3: every hour block is written to its file offset in parallel with pwrite.

use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::thread;

const THREADS: usize = 12; // 6 P-cores x 2 hardware threads
const CHUNK: usize = 1 << 20;
const TABLE: usize = 1 << 13; // interner slots; the catalogue has 1,842 endpoints
const HOURS: usize = 72;
const LOW: u64 = 0x0101_0101_0101_0101;

extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
}

type Name = &'static [u8];

/// Open-addressing endpoint table; slot value 0 = empty, else entry index + 1.
/// An endpoint is identified by its 64-bit hash plus its length, with no byte comparison:
/// that saves a `memcmp` per line (~10% of phase 1). For 1,842 names the chance of a
/// collision is about 1e-13. Raise `intern` to a full compare if that ever feels too bold.
struct Interner {
    slots: Vec<u32>,
    entries: Vec<(u64, Name)>,
}

impl Interner {
    fn new() -> Self {
        Interner {
            slots: vec![0; TABLE],
            entries: Vec::new(),
        }
    }
    fn intern(&mut self, h: u64, key: Name) -> u32 {
        let mut i = h as usize & (TABLE - 1);
        loop {
            let s = self.slots[i];
            if s == 0 {
                self.entries.push((h, key));
                self.slots[i] = self.entries.len() as u32;
                return self.entries.len() as u32 - 1;
            }
            let (eh, name) = self.entries[s as usize - 1];
            if eh == h && name.len() == key.len() {
                return s - 1;
            }
            i = (i + 1) & (TABLE - 1);
        }
    }
}

/// One thread's output of phase 1: its endpoint dictionary and records bucketed by hour.
/// Record = (local_endpoint_id << 1 | is_5xx) << 32 | latency_us.
struct Shard {
    names: Vec<(u64, Name)>,
    buckets: Vec<Vec<u64>>,
}

/// Eight bytes at `p`, zero-padded past the end of the file.
#[inline(always)]
fn word(d: &[u8], p: usize) -> u64 {
    match d.get(p..p + 8) {
        Some(s) => u64::from_le_bytes(s.try_into().unwrap()),
        None => d[p..].iter().rev().fold(0, |a, &b| a << 8 | b as u64),
    }
}

/// Nonzero iff the word has a space byte; the lowest set bit marks the first one exactly.
#[inline(always)]
fn space_mask(w: u64) -> u64 {
    let x = w ^ (LOW * 0x20);
    x.wrapping_sub(LOW) & !x & (LOW * 0x80)
}

#[inline(always)]
fn mix(h: u64, w: u64) -> u64 {
    (h.rotate_left(29) ^ w).wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

/// First line start at or after `pos`. Chunk c owns the lines starting in [c*CHUNK, (c+1)*CHUNK).
fn line_start(d: &[u8], pos: usize) -> usize {
    if pos == 0 {
        0
    } else if pos >= d.len() {
        d.len()
    } else {
        d[pos - 1..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(d.len(), |i| pos + i)
    }
}

fn ingest(d: Name, next: &AtomicUsize) -> Shard {
    let mut tab = Interner::new();
    // ~1.7x the expected share of records per bucket; untouched pages cost neither time nor memory.
    let cap = d.len() / 40 / (THREADS * HOURS);
    let mut buckets: Vec<Vec<u64>> = (0..HOURS).map(|_| Vec::with_capacity(cap)).collect();
    let chunks = d.len().div_ceil(CHUNK);
    loop {
        let c = next.fetch_add(1, Relaxed);
        if c >= chunks {
            break;
        }
        let (mut p, end) = (line_start(d, c * CHUNK), line_start(d, (c + 1) * CHUNK));
        while p < end {
            // "2026-MM-DDTHH:..." is always 27 bytes: month digit at 6, day at 8..10, hour at 11..13.
            let dg = |i: usize| (d[p + i] - b'0') as usize;
            let (day, hour) = (dg(8) * 10 + dg(9), dg(11) * 10 + dg(12));
            let ord = if d[p + 6] == b'0' { day - 30 } else { day + 1 }; // Oct 30 -> 0 .. Nov 2 -> 3
            let h = ord * 24 + hour - 18; // 0..=71
            p += 28;
            p += (space_mask(word(d, p)).trailing_zeros() / 8) as usize + 1; // skip method
            let start = p;
            let mut hash = 0x243F_6A88_85A3_08D3;
            loop {
                let w = word(d, p);
                let m = space_mask(w);
                if m == 0 {
                    hash = mix(hash, w);
                    p += 8;
                } else {
                    let k = (m.trailing_zeros() / 8) as usize;
                    hash = mix(hash, w & ((1u64 << (8 * k)) - 1));
                    p += k;
                    break;
                }
            }
            let id = tab.intern(hash ^ (hash >> 32), &d[start..p]);
            p += 1;
            let err = (d[p] == b'5') as u32; // only 5xx counts; 499 does not
            p += 4;
            let w = word(d, p);
            let x = w ^ (LOW * 0x0A);
            let m = x.wrapping_sub(LOW) & !x & (LOW * 0x80);
            let lat = if m != 0 {
                // 1..=7 digits: shift them to the top bytes so the low ones read as leading
                // zeros, then fold all eight digits with three multiplies.
                let k = (m.trailing_zeros() / 8) as usize;
                let mut v = (w << (64 - 8 * k)) & (LOW * 0x0F);
                v = (v * 2561) >> 8;
                v = ((v & 0x00FF_00FF_00FF_00FF) * 6_553_601) >> 16;
                v = ((v & 0x0000_FFFF_0000_FFFF) * 42_949_672_960_001) >> 32;
                p += k;
                v
            } else {
                // 8 to 10 digits (rare): plain loop.
                let mut v = 0u64;
                while d[p] != b'\n' {
                    v = v * 10 + (d[p] - b'0') as u64;
                    p += 1;
                }
                v
            };
            p += 1;
            buckets[h].push(((id << 1 | err) as u64) << 32 | lat);
        }
    }
    Shard {
        names: tab.entries,
        buckets,
    }
}

/// Nearest-rank p50/p95/p99. Small groups sort. Large ones select the ranks in ascending
/// order, each selection confined to what lies above the previous pivot (~1.5 passes).
fn percentiles(s: &mut [u32]) -> [u32; 3] {
    let n = s.len();
    let rank = |p: usize| (p * n + 99) / 100; // 1-indexed ceil(p*n/100)
    if n <= 24 {
        s.sort_unstable();
        return [s[rank(50) - 1], s[rank(95) - 1], s[rank(99) - 1]];
    }
    let (mut lo, mut last) = (0usize, (0usize, 0u32));
    let mut out = [0u32; 3];
    for (slot, p) in [(0, 50), (1, 95), (2, 99)] {
        let r = rank(p);
        if last.0 != r {
            last = (r, *s[lo..].select_nth_unstable(r - 1 - lo).1);
            lo = r;
        }
        out[slot] = last.1;
    }
    out
}

fn push_u(out: &mut Vec<u8>, mut v: u64) {
    let mut t = [0u8; 20];
    let mut i = 20;
    loop {
        i -= 1;
        t[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    out.extend_from_slice(&t[i..]);
}

/// Phase 2 worker: takes hours off the shared counter and renders each one's lines.
fn render(
    next: &AtomicUsize,
    shards: &[Shard],
    ranks: &[Vec<u32>],
    names: &[Name],
    hours: &[String],
) -> Vec<(usize, Vec<u8>)> {
    let e = names.len();
    let (mut cnt, mut cur, mut errs) = (vec![0u32; e + 1], vec![0u32; e], vec![0u32; e]);
    let (mut lat, mut done) = (Vec::new(), Vec::new());
    loop {
        let h = next.fetch_add(1, Relaxed);
        if h >= HOURS {
            return done;
        }
        cnt.fill(0);
        errs.fill(0);
        for (s, rk) in shards.iter().zip(ranks) {
            for &r in &s.buckets[h] {
                let k = (r >> 32) as usize;
                let g = rk[k >> 1] as usize;
                cnt[g + 1] += 1;
                errs[g] += (k & 1) as u32;
            }
        }
        for g in 0..e {
            cnt[g + 1] += cnt[g];
        }
        cur.copy_from_slice(&cnt[..e]);
        lat.clear();
        lat.resize(cnt[e] as usize, 0u32);
        for (s, rk) in shards.iter().zip(ranks) {
            for &r in &s.buckets[h] {
                let g = rk[(r >> 33) as usize] as usize;
                lat[cur[g] as usize] = r as u32;
                cur[g] += 1;
            }
        }
        let mut out = Vec::with_capacity(cnt[e] as usize * 4 + 4096);
        for g in 0..e {
            let (a, b) = (cnt[g] as usize, cnt[g + 1] as usize);
            if a == b {
                continue;
            }
            let p = percentiles(&mut lat[a..b]);
            let count = (b - a) as u64;
            out.extend_from_slice(hours[h].as_bytes());
            out.extend_from_slice(names[g]);
            for v in [
                count,
                p[0] as u64,
                p[1] as u64,
                p[2] as u64,
                errs[g] as u64,
                errs[g] as u64 * 1_000_000 / count,
            ] {
                out.push(b' ');
                push_u(&mut out, v);
            }
            out.push(b'\n');
        }
        done.push((h, out));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let input = args.get(1).map_or("/data/requests.log", |s| s);
    let output = args.get(2).map_or("/out/summary.txt", |s| s);
    let file = File::open(input).expect("open input");
    let len = file.metadata().unwrap().len() as usize;
    let out_file = File::create(output).expect("create output");
    if len == 0 {
        return;
    }
    // The mapping lives until exit, so the slice is 'static. Page-cache pages are shared, never copied.
    let d: Name = unsafe {
        let p = mmap(
            std::ptr::null_mut(),
            len,
            1, /* PROT_READ */
            2, /* MAP_PRIVATE */
            file.as_raw_fd(),
            0,
        );
        assert!(p as isize != -1, "mmap failed");
        std::slice::from_raw_parts(p, len)
    };

    let next = AtomicUsize::new(0);
    let shards: Vec<Shard> = thread::scope(|sc| {
        let hs: Vec<_> = (0..THREADS)
            .map(|_| sc.spawn(|| ingest(d, &next)))
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });

    // Unify the per-thread dictionaries, sort once bytewise (proper prefix first), map local id -> rank.
    let mut global = Interner::new();
    let remap: Vec<Vec<u32>> = shards
        .iter()
        .map(|s| s.names.iter().map(|&(h, n)| global.intern(h, n)).collect())
        .collect();
    let mut order: Vec<u32> = (0..global.entries.len() as u32).collect();
    order.sort_unstable_by_key(|&i| global.entries[i as usize].1);
    let mut rank_of = vec![0u32; order.len()];
    for (r, &i) in order.iter().enumerate() {
        rank_of[i as usize] = r as u32;
    }
    let names: Vec<Name> = order
        .iter()
        .map(|&i| global.entries[i as usize].1)
        .collect();
    let ranks: Vec<Vec<u32>> = remap
        .iter()
        .map(|m| m.iter().map(|&g| rank_of[g as usize]).collect())
        .collect();
    let hours: Vec<String> = (0..HOURS)
        .map(|h| {
            let date = ["2026-10-30", "2026-10-31", "2026-11-01", "2026-11-02"][(h + 18) / 24];
            format!("{date}T{:02}:00:00Z ", (h + 18) % 24)
        })
        .collect();

    let next = AtomicUsize::new(0);
    let parts: Vec<Vec<(usize, Vec<u8>)>> = thread::scope(|sc| {
        let hs: Vec<_> = (0..THREADS)
            .map(|_| sc.spawn(|| render(&next, &shards, &ranks, &names, &hours)))
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut blocks = vec![Vec::new(); HOURS];
    for (h, b) in parts.into_iter().flatten() {
        blocks[h] = b;
    }
    let mut offs = [0u64; HOURS];
    for h in 1..HOURS {
        offs[h] = offs[h - 1] + blocks[h - 1].len() as u64;
    }
    thread::scope(|sc| {
        for t in 0..THREADS {
            let (f, blocks, offs) = (&out_file, &blocks, &offs);
            sc.spawn(move || {
                for h in (t..HOURS).step_by(THREADS) {
                    f.write_all_at(&blocks[h], offs[h]).expect("write");
                }
            });
        }
    });
}