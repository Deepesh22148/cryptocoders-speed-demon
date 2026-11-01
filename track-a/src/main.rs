//! Track A (Ingest): per (hour, endpoint) count, nearest-rank p50/p95/p99, 5xx errors.
//!
//! Pipeline: 12 threads each parse a newline-aligned slice of the file into compact
//! (key, latency) records -> merge endpoint dictionaries and sort them bytewise ->
//! counting-sort latencies by group id -> select percentiles -> one write.
//! Group id = hour * E + endpoint_rank, so ascending group id IS the required output order.

use std::fs::File;
use std::os::unix::fs::FileExt;
use std::thread;

const THREADS: usize = 12; // 6 P-cores x 2 hardware threads
const BLOCK: usize = 1 << 20; // pread block size
const TABLE: usize = 1 << 14; // interner slots (1,842 endpoints -> load ~0.11)
const HOURS: usize = 72;

fn hash(b: &[u8]) -> u64 {
    let mut h = 0x9E37_79B9_7F4A_7C15 ^ b.len() as u64;
    for c in b.chunks(8) {
        let mut w = [0u8; 8];
        w[..c.len()].copy_from_slice(c);
        h = (h.rotate_left(5) ^ u64::from_le_bytes(w)).wrapping_mul(0x517C_C1B7_2722_0A95);
    }
    h ^ (h >> 32)
}

/// Open-addressing byte-string -> dense id table (slot value 0 = empty, else id + 1).
struct Interner {
    slots: Vec<u32>,
    hashes: Vec<u64>,
    names: Vec<Box<[u8]>>,
}

impl Interner {
    fn new() -> Self {
        Interner { slots: vec![0; TABLE], hashes: Vec::new(), names: Vec::new() }
    }
    fn intern(&mut self, key: &[u8]) -> u32 {
        let h = hash(key);
        let mut i = h as usize & (TABLE - 1);
        loop {
            let s = self.slots[i];
            if s == 0 {
                self.names.push(key.into());
                self.hashes.push(h);
                self.slots[i] = self.names.len() as u32;
                return self.names.len() as u32 - 1;
            }
            let id = (s - 1) as usize;
            if self.hashes[id] == h && *self.names[id] == *key {
                return s - 1;
            }
            i = (i + 1) & (TABLE - 1);
        }
    }
}

/// Record key layout: endpoint_local_id << 8 | hour_index << 1 | is_5xx.
type Rec = (u32, u32);

fn parse_block(data: &[u8], names: &mut Interner, recs: &mut Vec<Rec>) {
    for line in data.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        // The timestamp is always 27 bytes, so only fixed offsets are read:
        // "2026-MM-DDTHH:..." -> month at 5..7, day at 8..10, hour at 11..13.
        let d = |i: usize| (line[i] - b'0') as usize;
        let (day, hour) = (d(8) * 10 + d(9), d(11) * 10 + d(12));
        let ord = if line[6] == b'0' { day - 30 } else { day + 1 }; // Oct 30 -> 0 ... Nov 2 -> 3
        let h = ord * 24 + hour - 18; // 0..=71, Oct 30 18:00 is 0
        let rest = &line[28..]; // skip "<ts> "
        let m_end = rest.iter().position(|&b| b == b' ').unwrap();
        let rest = &rest[m_end + 1..]; // skip "<method> "
        let e_end = rest.iter().position(|&b| b == b' ').unwrap();
        let rest_after = &rest[e_end + 1..]; // "<status> <latency>"
        let err = (rest_after[0] == b'5') as u32; // only 5xx counts; 499 does not
        let lat = rest_after[4..].iter().fold(0u64, |a, &b| a * 10 + (b - b'0') as u64) as u32;
        let id = names.intern(&rest[..e_end]);
        recs.push((id << 8 | (h as u32) << 1 | err, lat));
    }
}

fn worker(file: &File, start: u64, end: u64) -> (Vec<Box<[u8]>>, Vec<Rec>) {
    let mut names = Interner::new();
    let mut recs = Vec::with_capacity(((end - start) / 60) as usize);
    let mut buf = vec![0u8; BLOCK + 1024];
    let (mut off, mut carry) = (start, 0usize);
    while off < end {
        let want = ((end - off) as usize).min(BLOCK);
        let n = file.read_at(&mut buf[carry..carry + want], off).expect("read");
        if n == 0 {
            break;
        }
        off += n as u64;
        let len = carry + n;
        let Some(last) = buf[..len].iter().rposition(|&b| b == b'\n') else {
            carry = len;
            continue;
        };
        parse_block(&buf[..=last], &mut names, &mut recs);
        buf.copy_within(last + 1..len, 0);
        carry = len - last - 1;
    }
    (names.names, recs)
}

/// First line start at or after `pos`, so every thread's range begins on a line.
fn line_start_at_or_after(file: &File, pos: u64, total: u64) -> u64 {
    if pos == 0 {
        return 0;
    }
    if pos >= total {
        return total;
    }
    let (mut p, mut b) = (pos - 1, [0u8; 4096]);
    loop {
        let n = file.read_at(&mut b, p).expect("read");
        if n == 0 {
            return total;
        }
        if let Some(i) = b[..n].iter().position(|&c| c == b'\n') {
            return p + i as u64 + 1;
        }
        p += n as u64;
    }
}

/// Nearest-rank p50/p95/p99 by selection; a finished selection bounds the next one.
fn percentiles(s: &mut [u32]) -> [u32; 3] {
    let n = s.len();
    let rank = |p: usize| (p * n + 99) / 100; // 1-indexed ceil(p*n/100)
    let (mut end, mut last) = (n, (0usize, 0u32));
    let mut out = [0u32; 3];
    for (slot, p) in [(2, 99), (1, 95), (0, 50)] {
        let r = rank(p);
        if last.0 != r {
            last = (r, *s[..end].select_nth_unstable(r - 1).1);
            end = r - 1;
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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let input = args.get(1).map_or("/data/requests.log", |s| s);
    let output = args.get(2).map_or("/out/summary.txt", |s| s);
    let file = File::open(input).expect("open input");
    let total = file.metadata().unwrap().len();

    let mut bounds: Vec<u64> =
        (0..THREADS).map(|i| line_start_at_or_after(&file, total * i as u64 / THREADS as u64, total)).collect();
    bounds.push(total);
    let results: Vec<(Vec<Box<[u8]>>, Vec<Rec>)> = thread::scope(|sc| {
        let hs: Vec<_> = (0..THREADS).map(|i| {
            let (f, a, b) = (&file, bounds[i], bounds[i + 1]);
            sc.spawn(move || worker(f, a, b))
        }).collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });

    // Global endpoint order: bytewise, proper prefix first (Vec<u8> ordering is exactly that).
    let mut all: Vec<&[u8]> = results.iter().flat_map(|(n, _)| n.iter().map(|b| &**b)).collect();
    all.sort_unstable();
    all.dedup();
    let e = all.len();
    let ranks: Vec<Vec<u32>> = results.iter()
        .map(|(n, _)| n.iter().map(|b| all.binary_search(&&**b).unwrap() as u32).collect())
        .collect();
    let gid = |t: usize, k: u32| ((k >> 1) & 127) as usize * e + ranks[t][(k >> 8) as usize] as usize;

    // Counting sort of latencies by group id.
    let groups = HOURS * e;
    let mut starts = vec![0u32; groups + 1];
    let mut errors = vec![0u32; groups];
    for (t, (_, recs)) in results.iter().enumerate() {
        for &(k, _) in recs {
            let g = gid(t, k);
            starts[g + 1] += 1;
            errors[g] += k & 1;
        }
    }
    for g in 0..groups {
        starts[g + 1] += starts[g];
    }
    let mut cursor = starts.clone();
    let mut lat = vec![0u32; starts[groups] as usize];
    for (t, (_, recs)) in results.iter().enumerate() {
        for &(k, l) in recs {
            let g = gid(t, k);
            lat[cursor[g] as usize] = l;
            cursor[g] += 1;
        }
    }

    let hour_str: Vec<String> = (0..HOURS).map(|h| {
        let (day, hh) = ((h + 18) / 24, (h + 18) % 24);
        let date = ["2026-10-30", "2026-10-31", "2026-11-01", "2026-11-02"][day];
        format!("{date}T{hh:02}:00:00Z ")
    }).collect();

    let mut out = Vec::with_capacity(12 << 20);
    for g in 0..groups {
        let (a, b) = (starts[g] as usize, starts[g + 1] as usize);
        if a == b {
            continue;
        }
        let p = percentiles(&mut lat[a..b]);
        let count = (b - a) as u64;
        out.extend_from_slice(hour_str[g / e].as_bytes());
        out.extend_from_slice(all[g % e]);
        for v in [count, p[0] as u64, p[1] as u64, p[2] as u64, errors[g] as u64, errors[g] as u64 * 1_000_000 / count] {
            out.push(b' ');
            push_u(&mut out, v);
        }
        out.push(b'\n');
    }
    std::fs::write(output, out).expect("write output");
}