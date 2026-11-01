//! Track A (Ingest): per (hour, endpoint) count, nearest-rank p50/p95/p99, 5xx errors.
//!
//! Phase 1 (12 threads, dynamic 1 MB chunks of an mmap'd file): one forward pass per line,
//!   fixed-offset hour (re-derived only when the "MM-DDTHH" bytes change), SWAR scans for the
//!   method and endpoint (hashed as it goes), SWAR latency parse, one 6-byte record pushed
//!   into the chunk's segment for that hour. The file is nearly time-ordered (a 1 MB chunk
//!   touches 1-2 hours), so a chunk owns one exactly-bounded segment per hour it sees and
//!   nothing ever regrows. All loads are plain unaligned reads: the mapping is followed by a
//!   page of zeros, so reading a few bytes past the file's end is safe.
//! Merge (tiny): endpoint dictionaries are unified and sorted bytewise once.
//! Phase 2 (12 threads, dynamic, one hour per work item): counting-sort that hour's records
//!   by endpoint rank in an L2-resident scratch array, select the percentiles, assemble each
//!   output line in a stack buffer (two digits per division) and append it. A finished hour
//!   block is parked in the sink until all earlier hours are written, so no worker ever
//!   waits and each block lands at its exact file offset with pwrite.
//! The process exits right after the last write: tearing down ~25 MB of segments one munmap
//! at a time would only delay it.

use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicUsize, Ordering::*};
use std::sync::Mutex;
use std::thread;

const THREADS: usize = 12; // 6 P-cores x 2 hardware threads
const CHUNK: usize = 1 << 20;
const TABLE: usize = 1 << 13; // interner slots; the catalogue has 1,842 endpoints
const HOURS: usize = 72;
const LOW: u64 = 0x0101_0101_0101_0101;
/// Room for one output line: 21 (hour) + 125 (longest endpoint) + 6 numbers of up to 10 digits + spaces.
const LINE: usize = 256;

extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
}

/// Map the file read-only, followed by a page of anonymous zeros so 8-byte loads at the last
/// bytes of the file stay inside mapped memory.
fn map_file(file: &File, len: usize) -> Name {
    const PAGE: usize = 4096;
    let total = (len + PAGE - 1) / PAGE * PAGE + PAGE;
    unsafe {
        // 3 = PROT_READ|PROT_WRITE, 0x22 = MAP_PRIVATE|MAP_ANONYMOUS: reserve the whole range first.
        let base = mmap(std::ptr::null_mut(), total, 3, 0x22, -1, 0);
        assert!(base as isize != -1, "mmap failed");
        // 1 = PROT_READ, 0x12 = MAP_PRIVATE|MAP_FIXED: lay the file over its start.
        let p = mmap(base, len, 1, 0x12, file.as_raw_fd(), 0);
        assert!(p == base, "mmap of the input failed");
        std::slice::from_raw_parts(p, len)
    }
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

/// One request, packed to 6 bytes (3.1M of them are the bulk of the memory).
/// `key` = local endpoint id << 1 | is_5xx.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct Rec {
    lat: u32,
    key: u16,
}

/// One thread's output of phase 1: its endpoint dictionary and its record segments,
/// each holding the records of one hour from one chunk.
struct Shard {
    names: Vec<(u64, Name)>,
    segs: Vec<(usize, Vec<Rec>)>,
}

/// The open segment of `hour` in this chunk, created on first use. Every line is at least
/// 40 bytes, so `cap` records is a hard bound and the segment never reallocates.
fn open_seg(open: &mut Vec<(usize, Vec<Rec>)>, hour: usize, cap: usize) -> usize {
    open.iter().position(|s| s.0 == hour).unwrap_or_else(|| {
        open.push((hour, Vec::with_capacity(cap)));
        open.len() - 1
    })
}

/// Eight bytes at `p`, a plain load. `main` maps the file followed by a page of zeros, so any
/// `p` below the file length is safe (a line's scans never start past its own end).
#[inline(always)]
fn word(d: &[u8], p: usize) -> u64 {
    debug_assert!(p < d.len());
    unsafe { (d.as_ptr().add(p) as *const u64).read_unaligned() }
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
    let mut segs = Vec::new();
    let chunks = d.len().div_ceil(CHUNK);
    loop {
        let c = next.fetch_add(1, Relaxed);
        if c >= chunks {
            break;
        }
        let (mut p, end) = (line_start(d, c * CHUNK), line_start(d, (c + 1) * CHUNK));
        let cap = (end - p) / 40 + 1;
        let mut open: Vec<(usize, Vec<Rec>)> = Vec::new();
        let (mut cur, mut cur_pw) = (0, u64::MAX);
        while p < end {
            // "2026-MM-DDTHH:..." is always 27 bytes: month digit at 6, day at 8..10, hour at 11..13.
            // Consecutive lines nearly always share the hour, so the hour is only worked out when
            // the "MM-DDTHH" bytes (timestamp 5..13) differ from the previous line's.
            let pw = word(d, p + 5);
            if pw != cur_pw {
                let dg = |i: usize| (d[p + i] - b'0') as usize;
                let (day, hour) = (dg(8) * 10 + dg(9), dg(11) * 10 + dg(12));
                let ord = if d[p + 6] == b'0' { day - 30 } else { day + 1 }; // Oct 30 -> 0 .. Nov 2 -> 3
                let h = ord * 24 + hour - 18; // 0..=71
                (cur, cur_pw) = (open_seg(&mut open, h, cap), pw);
            }
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
            open[cur].1.push(Rec {
                lat: lat as u32,
                key: (id << 1 | err) as u16,
            });
        }
        segs.append(&mut open);
    }
    Shard {
        names: tab.entries,
        segs,
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

/// "00".."99" as 200 bytes, so two digits are written per division.
const PAIRS: [u8; 200] = {
    let mut t = [0u8; 200];
    let mut i = 0;
    while i < 100 {
        t[2 * i] = b'0' + (i / 10) as u8;
        t[2 * i + 1] = b'0' + (i % 10) as u8;
        i += 1;
    }
    t
};

/// Write `v` in decimal at `line[*n..]` and advance `*n`. The digit count is known up front, so
/// the digits are written back to front, two at a time.
#[inline(always)]
fn put_u(line: &mut [u8; LINE], n: &mut usize, mut v: u32) {
    let len = v.checked_ilog10().map_or(1, |l| l as usize + 1);
    let mut i = *n + len;
    while v >= 100 {
        let r = (v % 100) as usize * 2;
        v /= 100;
        i -= 2;
        line[i..i + 2].copy_from_slice(&PAIRS[r..r + 2]);
    }
    if v >= 10 {
        line[i - 2..i].copy_from_slice(&PAIRS[v as usize * 2..v as usize * 2 + 2]);
    } else {
        line[i - 1] = b'0' + v as u8;
    }
    *n += len;
}

/// Phase 2 worker: takes hours off the shared counter, renders each one's lines and writes
/// them at the offset where the previous hour ended (`ends[h]` = end offset, MAX = unknown yet).
fn render(
    next: &AtomicUsize,
    hour_segs: &[Vec<(usize, &[Rec])>],
    ranks: &[Vec<u32>],
    names: &[Name],
    hours: &[[u8; 21]],
    sink: &Mutex<Sink>,
) {
    let e = names.len();
    let (mut cnt, mut cur, mut errs) = (vec![0u32; e + 1], vec![0u32; e], vec![0u32; e]);
    let mut lat = Vec::new();
    loop {
        let h = next.fetch_add(1, Relaxed);
        if h >= HOURS {
            return;
        }
        cnt.fill(0);
        errs.fill(0);
        for &(t, seg) in &hour_segs[h] {
            let rk = &ranks[t];
            for &r in seg {
                let g = rk[(r.key >> 1) as usize] as usize;
                cnt[g + 1] += 1;
                errs[g] += (r.key & 1) as u32;
            }
        }
        for g in 0..e {
            cnt[g + 1] += cnt[g];
        }
        cur.copy_from_slice(&cnt[..e]);
        lat.clear();
        lat.resize(cnt[e] as usize, 0u32);
        for &(t, seg) in &hour_segs[h] {
            let rk = &ranks[t];
            for &r in seg {
                let g = rk[(r.key >> 1) as usize] as usize;
                lat[cur[g] as usize] = r.lat;
                cur[g] += 1;
            }
        }
        let mut out = sink.lock().unwrap().pool.pop().unwrap_or_default();
        for g in 0..e {
            let (a, b) = (cnt[g] as usize, cnt[g + 1] as usize);
            if a == b {
                continue;
            }
            let p = percentiles(&mut lat[a..b]);
            let count = (b - a) as u32;
            let mut line = [0u8; LINE];
            line[..21].copy_from_slice(&hours[h]);
            let name = names[g];
            // A fixed 128-byte copy (the mapping has padding after the file) beats a memcpy call;
            // the spaces and digits written next overwrite the surplus.
            unsafe { std::ptr::copy_nonoverlapping(name.as_ptr(), line.as_mut_ptr().add(21), 128) };
            let mut n = 21 + name.len();
            let errors = errs[g];
            for v in [
                count,
                p[0],
                p[1],
                p[2],
                errors,
                (errors as u64 * 1_000_000 / count as u64) as u32,
            ] {
                line[n] = b' ';
                n += 1;
                put_u(&mut line, &mut n, v);
            }
            line[n] = b'\n';
            out.extend_from_slice(&line[..n + 1]);
        }
        let mut guard = sink.lock().unwrap();
        let s = &mut *guard;
        s.slots[h] = Some(out);
        while s.next < HOURS {
            let Some(mut b) = s.slots[s.next].take() else {
                break;
            };
            s.file.write_all_at(&b, s.off).expect("write");
            s.off += b.len() as u64;
            s.next += 1;
            b.clear();
            s.pool.push(b);
        }
    }
}

/// Finished hour blocks, parked until every earlier hour has been written, so no worker ever waits.
struct Sink<'a> {
    file: &'a File,
    next: usize,
    off: u64,
    slots: Vec<Option<Vec<u8>>>,
    pool: Vec<Vec<u8>>,
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
    let d: Name = map_file(&file, len);

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
    let hours: Vec<[u8; 21]> = (0..HOURS)
        .map(|h| {
            let date = ["2026-10-30", "2026-10-31", "2026-11-01", "2026-11-02"][(h + 18) / 24];
            format!("{date}T{:02}:00:00Z ", (h + 18) % 24)
                .into_bytes()
                .try_into()
                .unwrap()
        })
        .collect();

    let mut hour_segs: Vec<Vec<(usize, &[Rec])>> = vec![Vec::new(); HOURS];
    for (t, s) in shards.iter().enumerate() {
        for (h, seg) in &s.segs {
            hour_segs[*h].push((t, seg));
        }
    }
    let next = AtomicUsize::new(0);
    let sink = Mutex::new(Sink {
        file: &out_file,
        next: 0,
        off: 0,
        slots: vec![None; HOURS],
        pool: vec![],
    });
    thread::scope(|sc| {
        for _ in 0..THREADS {
            sc.spawn(|| render(&next, &hour_segs, &ranks, &names, &hours, &sink));
        }
    });
    // Everything is written; skip freeing ~25 MB of segments one munmap at a time.
    std::process::exit(0);
}