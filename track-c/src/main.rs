use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

const BOM: u8 = 0xEF; // first byte of EF BB BF; a record never starts with it otherwise
const SLOTS: usize = 1 << 17;
const ONES: u64 = 0x0101_0101_0101_0101;
const CHUNK: usize = 1 << 16;
const RING: usize = 16; // records between a name's prefetch and its table update

// Name table shared by all threads, open addressing: [name hash, 256-bit set of groups].
// ponytail: names are keyed by a 64-bit hash, not bytes (~1e-10 collision odds on the
// spec's 100k-name pool); 131k slots hold that pool, a bigger pool needs more slots.
type Table = [[AtomicU64; 5]];
type Agg = [i64; 8]; // count, id sum, amount sum, min, max, name bytes, note bytes, names

// Hints that *p is needed soon; `write` asks for the line in exclusive state (prefetchw),
// which saves a second cache-coherence round trip before an atomic update.
fn prefetch<T>(p: *const T, write: bool) {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        match write {
            true => std::arch::asm!("prefetchw [{}]", in(reg) p, options(nostack, readonly)),
            false => std::arch::asm!("prefetcht0 [{}]", in(reg) p, options(nostack, readonly)),
        }
    }
}

// Bitmasks of the quotes, commas and LFs in 64 bytes: bit i is byte i.
#[cfg(target_arch = "x86_64")]
fn masks(w: &[u8; 64]) -> [u64; 3] {
    use std::arch::x86_64::*;
    let mut m = [0; 3];
    for i in 0..4 {
        let v = unsafe { _mm_loadu_si128(w.as_ptr().add(16 * i) as *const __m128i) };
        for (k, c) in [b'"', b',', b'\n'].into_iter().enumerate() {
            let bits = unsafe { _mm_movemask_epi8(_mm_cmpeq_epi8(v, _mm_set1_epi8(c as i8))) };
            m[k] |= (bits as u16 as u64) << (16 * i);
        }
    }
    m
}

#[cfg(not(target_arch = "x86_64"))]
fn masks(w: &[u8; 64]) -> [u64; 3] {
    [b'"', b',', b'\n'].map(|c| w.iter().rev().fold(0, |m, &x| m << 1 | (x == c) as u64))
}

// The integer in b[s..e], eight digits at a time (SWAR) when eight bytes are readable.
fn int(b: &[u8], s: usize, e: usize) -> i64 {
    let n = e - s;
    match b.get(s..s + 8) {
        Some(w) if (1..=8).contains(&n) => {
            let mut v = (u64::from_le_bytes(w.try_into().unwrap()) ^ 0x30 * ONES) << (64 - 8 * n);
            v = (v * 10 + (v >> 8)) & 0x00FF_00FF_00FF_00FF;
            v = (v * 100 + (v >> 16)) & 0x0000_FFFF_0000_FFFF;
            ((v * 10000 + (v >> 32)) & 0xFFFF_FFFF) as i64
        }
        _ => b[s..e].iter().fold(0, |v, &c| v * 10 + (c - b'0') as i64),
    }
}

// Hash of b[s..e]: the first and last eight bytes (overlapping), and every eight bytes between
// for long names. The record's later fields always follow, so eight bytes from s are readable.
fn hash(b: &[u8], s: usize, e: usize) -> u64 {
    let w = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
    let n = e - s;
    let (mut h, last) = match n {
        0 => (0, 0),
        1..8 => (w(s) & u64::MAX >> (64 - 8 * n), 0),
        _ => (w(s), w(e - 8)),
    };
    for i in (s + 8..e.saturating_sub(8)).step_by(8) {
        h = h.wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(29) ^ w(i);
    }
    h = (h.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ last ^ (n as u64) << 58).wrapping_mul(0xD6E8_FEB8_6659_FD93);
    (h ^ h >> 32).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1
}

// Sets group g's bit for name h; true if this call set it, so each distinct pair counts once.
fn mark(t: &Table, h: u64, g: usize) -> bool {
    let mut i = (h >> 47) as usize;
    loop {
        let k = t[i][0].load(Relaxed);
        if k == h || k == 0 && t[i][0].compare_exchange(0, h, Relaxed, Relaxed).map_or_else(|x| x == h, |_| true) {
            let (w, bit) = (&t[i][1 + g / 64], 1 << (g % 64));
            return w.fetch_or(bit, Relaxed) & bit == 0;
        }
        i = (i + 1) % SLOTS;
    }
}

// One record: it starts at s, its four field commas are at c (with q[k] quotes before c[k]),
// and its terminator LF is at e (or e is the end of the file). A field with n quotes is
// unescaped to its raw length minus n / 2, minus one more when it is quoted at all.
fn record(b: &[u8], s: usize, c: [usize; 4], q: [u64; 4], e: usize, region: &[u8], aggs: &mut [Agg], t: &Table) -> (u64, usize) {
    let s = s + 3 * (b[s] == BOM) as usize;
    let len = |k: usize| (c[k + 1] - c[k] - 1) as i64 - (q[k + 1] - q[k]) as i64 / 2 - (q[k + 1] > q[k]) as i64;
    let (name, note, quoted) = (len(0), len(1), (q[1] > q[0]) as usize);
    let h = hash(b, c[0] + 1 + quoted, c[1] - quoted);
    let slot = &t[(h >> 47) as usize];
    (prefetch(&slot[0], true), prefetch(&slot[4], true));
    let rid = int(b, c[2] + 1, c[3]) as usize;
    let e = e - (b[e - 1] == b'\r') as usize;
    let neg = b[c[3] + 1] == b'-';
    let v = (int(b, c[3] + 1 + neg as usize, e - 3) * 100 + int(b, e - 2, e)) * if neg { -1 } else { 1 };
    let g = match region.get(rid) {
        Some(&g) if g != u8::MAX => g as usize,
        _ => aggs.len() - 1,
    };
    let a = &mut aggs[g];
    for (i, x) in [(0, 1), (1, int(b, s, c[0])), (2, v), (5, name), (6, note)] {
        a[i] += x;
    }
    (a[3], a[4]) = (a[3].min(v), a[4].max(v));
    (h, g)
}

// Per-thread parse state: aggregates, plus (name, group) pairs waiting RING records for their
// table slot prefetch to land before the update.
struct Scan<'a> {
    region: &'a [u8],
    t: &'a Table,
    aggs: Vec<Agg>,
    ring: [(u64, usize); RING],
    n: usize,
}

impl Scan<'_> {
    fn defer(&mut self, (h, g): (u64, usize)) {
        let (h, g) = std::mem::replace(&mut self.ring[self.n % RING], (h, g));
        if self.n >= RING {
            self.aggs[g][7] += mark(self.t, h, g) as i64;
        }
        self.n += 1;
    }

    // Parses the complete records in b that start before lim, 64 bytes at a time; b starts at a
    // record. A prefix XOR of the quote mask marks the bytes inside quotes ("" flips twice, so
    // escapes need no care); commas and LFs outside quotes are the field and record boundaries.
    // Returns where an unfinished record starts when b ends before the file does.
    fn blocks(&mut self, b: &[u8], eof: bool, lim: usize) -> Option<usize> {
        let (mut s, mut c, mut q, mut k) = (0, [0; 4], [0; 4], 0);
        let (mut at, mut quotes, mut inside) = (0, 0, 0u64);
        while s < lim {
            let mut pad = [0; 64];
            let w: &[u8; 64] = match b.get(at..at + 64) {
                Some(w) => w.try_into().unwrap(),
                None if !eof => return Some(s),
                None if at < b.len() => {
                    pad[..b.len() - at].copy_from_slice(&b[at..]);
                    &pad
                }
                None => {
                    // Only the file's final record may lack its terminator.
                    assert!(k == 4, "bad record");
                    let hg = record(b, s, c, q, b.len(), self.region, &mut self.aggs, self.t);
                    self.defer(hg);
                    break;
                }
            };
            let [qm, cm, lm] = masks(w);
            let mut x = qm;
            for i in [1, 2, 4, 8, 16, 32] {
                x ^= x << i;
            }
            x ^= inside;
            inside = ((x as i64) >> 63) as u64;
            let mut bits = (cm | lm) & !x;
            while bits != 0 && s < lim {
                let i = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                let qn = quotes + (qm & ((1 << i) - 1)).count_ones() as u64;
                if lm >> i & 1 == 0 {
                    (c[k], q[k], k) = (at + i, qn, k + 1); // a fifth comma panics here
                    continue;
                }
                assert!(k == 4, "bad record");
                let hg = record(b, s, c, q, at + i, self.region, &mut self.aggs, self.t);
                self.defer(hg);
                (s, k) = (at + i + 1, 0);
            }
            (at, quotes) = (at + 64, quotes + qm.count_ones() as u64);
        }
        None
    }
}

// Streams the records that start in [start, end) through one buffer; on refill the unfinished
// record moves to the front and is scanned again. The last record may run past end.
fn scan(path: &str, start: usize, end: usize, region: &[u8], t: &Table, groups: usize) -> Vec<Agg> {
    let aggs = vec![[0, 0, 0, i64::MAX, i64::MIN, 0, 0, 0]; groups];
    let mut st = Scan { region, t, aggs, ring: [(0, 0); RING], n: 0 };
    let mut f = File::open(path).unwrap();
    f.seek(SeekFrom::Start(start as u64)).unwrap();
    let (mut buf, mut n, mut base) = (vec![0; CHUNK], 0, start);
    loop {
        let got = f.read(&mut buf[n..]).unwrap();
        n += got;
        let Some(s) = st.blocks(&buf[..n], got == 0, end - base) else { break };
        buf.copy_within(s..n, 0);
        (n, base) = (n - s, base + s);
    }
    for &(h, g) in &st.ring[..st.n.min(RING)] {
        st.aggs[g][7] += mark(t, h, g) as i64;
    }
    st.aggs
}

// First record start at or after `at`: an LF followed by a digit or a BOM. Inside quotes an
// LF only occurs in notes, which are lowercase words (spec 2.2), so this is never data.
fn boundary(path: &str, at: usize) -> usize {
    let mut f = File::open(path).unwrap();
    f.seek(SeekFrom::Start(at as u64 - 1)).unwrap();
    let mut w = Vec::new();
    f.take(1 << 16).read_to_end(&mut w).unwrap();
    let start = |x: &[u8]| x[0] == b'\n' && (x[1].is_ascii_digit() || x[1] == BOM);
    w.windows(2).position(start).map_or(usize::MAX, |i| at + i)
}

fn money(c: i64) -> String {
    let sign = if c < 0 { "-" } else { "" };
    format!("{sign}{}.{:02}", c.abs() / 100, c.abs() % 100)
}

fn main() {
    let dir = |var, default: &str| std::env::var(var).unwrap_or(default.into());
    let data = dir("SPEED_DEMON_DATA_DIR", "/data");
    let out_dir = dir("SPEED_DEMON_OUTPUT_DIR", "/out");
    let threads = std::thread::available_parallelism().map_or(12, |n| n.get());

    // Groups are the sorted country codes, then _UNMATCHED ('_' sorts after 'Z'). A code is
    // [A-Z]{3}, so its base-26 value orders codes the same way the bytes do.
    let (keys, region) = {
        let rtext = std::fs::read(format!("{data}/regions.csv")).unwrap();
        let rows = || rtext.split(|&c| c == b'\n').skip(1).filter(|l| l.len() > 4);
        let code = |l: &[u8]| l[l.len() - 3..].iter().fold(0, |a, &c| a * 26 + (c - b'A') as usize);
        let mut rank = vec![-1; 26 * 26 * 26];
        rows().for_each(|l| rank[code(l)] = 0);
        let mut keys = Vec::new();
        for (c, r) in rank.iter_mut().enumerate().filter(|(_, r)| **r == 0) {
            *r = keys.len() as i32;
            keys.push([c / 676, c / 26 % 26, c % 26].map(|x| (b'A' + x as u8) as char).iter().collect());
        }
        let mut region = vec![u8::MAX; 1 << 20];
        for l in rows() {
            let r = l[..l.len() - 4].iter().fold(0, |a, &c| a * 10 + (c - b'0') as usize);
            region.resize(region.len().max(r + 1), u8::MAX);
            region[r] = rank[code(l)] as u8;
        }
        keys.push("_UNMATCHED".to_string());
        (keys, region)
    };
    assert!(keys.len() < 256, "too many countries");

    // records.csv: skip the BOM and header, then cut at record starts, one range per thread.
    let path = format!("{data}/records.csv");
    let len = std::fs::metadata(&path).unwrap().len() as usize;
    let mut head = [0; 64];
    File::open(&path).unwrap().read_exact(&mut head).unwrap();
    let skip = 3 * (head[0] == BOM) as usize;
    assert!(head[skip..].starts_with(b"id,name,note,region_id,amount"));
    let mut cuts = vec![skip + head[skip..].iter().position(|&c| c == b'\n').unwrap() + 1];
    for t in 1..threads {
        let at = (len * t / threads).max(cuts[0] + 1);
        cuts.push(boundary(&path, at).clamp(cuts[cuts.len() - 1], len));
    }
    cuts.push(len);

    let table: Vec<[AtomicU64; 5]> = (0..SLOTS).map(|_| Default::default()).collect();
    let parts: Vec<Vec<Agg>> = std::thread::scope(|s| {
        let hs: Vec<_> = cuts.windows(2).map(|w| s.spawn(|| scan(&path, w[0], w[1], &region, &table, keys.len()))).collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });

    let mut out = Vec::new();
    for (g, key) in keys.iter().enumerate() {
        let sum = |i| parts.iter().map(|p| p[g][i]).sum::<i64>();
        let min = parts.iter().map(|p| p[g][3]).min().unwrap();
        let max = parts.iter().map(|p| p[g][4]).max().unwrap();
        if sum(0) > 0 {
            let (n, ids, total, nb, tb, d) = (sum(0), sum(1), money(sum(2)), sum(5), sum(6), sum(7));
            let (min, max) = (money(min), money(max));
            writeln!(out, "{key} {n} {ids} {total} {min} {max} {d} {nb} {tb}").unwrap();
        }
    }
    std::fs::create_dir_all(&out_dir).unwrap();
    std::fs::write(format!("{out_dir}/aggregates.txt"), out).unwrap();
}
