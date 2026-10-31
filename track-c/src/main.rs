use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

const BOM: u8 = 0xEF; // first byte of EF BB BF; a record never starts with it otherwise
const CHUNK: usize = 1 << 15;
const SLOTS: usize = 1 << 17;
const ONES: u64 = 0x0101_0101_0101_0101;


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

fn eat(b: &[u8], p: &mut usize, c: u8) -> Option<()> {
    (*b.get(*p)? == c).then(|| *p += 1)
}

// Up to seven digits at once (SWAR); longer runs and the buffer's last bytes take the loop.
fn num(b: &[u8], p: &mut usize) -> Option<i64> {
    if let Some(w) = b.get(*p..*p + 8) {
        let x = u64::from_le_bytes(w.try_into().unwrap()) ^ 0x30 * ONES;
        let n = ((x.wrapping_add(0x76 * ONES) | x) & ONES << 7).trailing_zeros() as usize / 8;
        if (1..8).contains(&n) {
            let mut v = x << (64 - 8 * n);
            v = (v * 10 + (v >> 8)) & 0x00FF_00FF_00FF_00FF;
            v = (v * 100 + (v >> 16)) & 0x0000_FFFF_0000_FFFF;
            *p += n;
            return Some(((v * 10000 + (v >> 32)) & 0xFFFF_FFFF) as i64);
        }
    }
    let mut v = 0;
    while b.get(*p)?.is_ascii_digit() {
        (v, *p) = (v * 10 + (b[*p] - b'0') as i64, *p + 1);
    }
    Some(v)
}

// The n <= 8 bytes at i as a little-endian word, zero-padded.
fn word(b: &[u8], i: usize, n: usize) -> u64 {
    let w = b.get(i..i + 8).map_or_else(|| b[i..i + n].iter().rev().fold(0, |w, &c| w << 8 | c as u64), |w| u64::from_le_bytes(w.try_into().unwrap()));
    w & u64::MAX >> (64 - 8 * n)
}

// Index of the first c at or after i, eight bytes at a time.
fn find(b: &[u8], mut i: usize, c: u8) -> Option<usize> {
    while i + 8 <= b.len() {
        let x = word(b, i, 8) ^ c as u64 * ONES;
        let m = x.wrapping_sub(ONES) & !x & ONES << 7;
        if m != 0 {
            return Some(i + m.trailing_zeros() as usize / 8);
        }
        i += 8;
    }
    b[i..].iter().position(|&x| x == c).map(|k| i + k)
}

// A text field and its comma: (raw start, raw end, unescaped length). The raw bytes map
// one to one to the unescaped value, and "" and an empty field are both empty.
fn text(b: &[u8], p: &mut usize) -> Option<(usize, usize, i64)> {
    if *b.get(*p)? != b'"' {
        let (s, e) = (*p, find(b, *p, b',')?);
        *p = e + 1;
        return Some((s, e, (e - s) as i64));
    }
    let (s, mut e, mut esc) = (*p + 1, *p + 1, 0);
    while {
        e = find(b, e, b'"')?;
        *b.get(e + 1)? == b'"'
    } {
        (e, esc) = (e + 2, esc + 1);
    }
    *p = e + 1;
    eat(b, p, b',')?;
    Some((s, e, (e - s - esc) as i64))
}

fn hash(b: &[u8], s: usize, e: usize) -> u64 {
    let mut h = (e - s) as u64;
    for i in (s..e).step_by(8) {
        h = (h ^ word(b, i, (e - i).min(8))).wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(29);
    }
    (h ^ h >> 32).wrapping_mul(0xD6E8_FEB8_6659_FD93) | 1
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

// One record at p: returns the offset after its terminator, or None if the buffer ends first.
// Table and region loads are prefetched early so their cache misses overlap the parsing.
fn record(b: &[u8], mut p: usize, region: &[u8], t: &Table, aggs: &mut [Agg]) -> Option<usize> {
    p += 3 * (*b.get(p)? == BOM) as usize;
    let id = num(b, &mut p)?;
    eat(b, &mut p, b',')?;
    let (s, e, name) = text(b, &mut p)?;
    let h = hash(b, s, e);
    let slot = &t[(h >> 47) as usize];
    (prefetch(&slot[0], true), prefetch(&slot[4], true));
    let note = text(b, &mut p)?.2;
    let rid = num(b, &mut p)? as usize;
    prefetch(region.as_ptr().wrapping_add(rid), false);
    eat(b, &mut p, b',')?;
    let neg = eat(b, &mut p, b'-').is_some();
    let whole = num(b, &mut p)?;
    eat(b, &mut p, b'.')?;
    let v = (whole * 100 + num(b, &mut p)?) * if neg { -1 } else { 1 };
    let _ = eat(b, &mut p, b'\r');
    eat(b, &mut p, b'\n')?;
    let g = match region.get(rid) {
        Some(&g) if g != u8::MAX => g as usize,
        _ => aggs.len() - 1,
    };
    let a = &mut aggs[g];
    for (i, x) in [(0, 1), (1, id), (2, v), (5, name), (6, note)] {
        a[i] += x;
    }
    (a[3], a[4]) = (a[3].min(v), a[4].max(v));
    a[7] += mark(t, h, g) as i64;
    Some(p)
}

// Streams the records that start in [start, end) through one buffer; the last may run past end.
fn scan(path: &str, start: u64, end: u64, region: &[u8], t: &Table, groups: usize) -> Vec<Agg> {
    let mut aggs = vec![[0, 0, 0, i64::MAX, i64::MIN, 0, 0, 0]; groups];
    let mut f = File::open(path).unwrap();
    f.seek(SeekFrom::Start(start)).unwrap();
    let (mut buf, mut n, mut pos, mut base) = (vec![0; CHUNK], 0, 0, start);
    while base + (pos as u64) < end {
        match record(&buf[..n], pos, region, t, &mut aggs) {
            Some(p) => pos = p,
            None => {
                buf.copy_within(pos..n, 0);
                (base, n, pos) = (base + pos as u64, n - pos, 0);
                let got = f.read(&mut buf[n..]).unwrap();
                if got == 0 {
                    assert!(n > 0 && buf[n - 1] != b'\n', "bad record at byte {base}");
                    buf[n] = b'\n'; // the final record may lack its terminator
                }
                n += got.max(1);
            }
        }
    }
    aggs
}

// First record start at or after `at`: an LF followed by a digit or a BOM. Inside quotes an
// LF only occurs in notes, which are lowercase words (spec 2.2), so this is never data.
fn boundary(path: &str, at: u64, len: u64) -> u64 {
    let mut f = File::open(path).unwrap();
    f.seek(SeekFrom::Start(at - 1)).unwrap();
    let mut w = Vec::new();
    f.take(1 << 16).read_to_end(&mut w).unwrap();
    let start = |x: &[u8]| x[0] == b'\n' && (x[1].is_ascii_digit() || x[1] == BOM);
    w.windows(2).position(start).map_or(len, |i| at + i as u64)
}

fn money(c: i64) -> String {
    let sign = if c < 0 { "-" } else { "" };
    format!("{sign}{}.{:02}", c.abs() / 100, c.abs() % 100)
}

fn main() {
    let dir = |var, default: &str| std::env::var(var).unwrap_or(default.into());
    let data = dir("SPEED_DEMON_DATA_DIR", "/data");
    let out_dir = dir("SPEED_DEMON_OUTPUT_DIR", "/out");
    let threads = std::thread::available_parallelism().map_or(12, |n| n.get()) as u64;

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
    let len = std::fs::metadata(&path).unwrap().len();
    let mut head = [0; 64];
    File::open(&path).unwrap().read_exact(&mut head).unwrap();
    let skip = 3 * (head[0] == BOM) as usize;
    assert!(head[skip..].starts_with(b"id,name,note,region_id,amount"));
    let mut cuts = vec![(skip + head[skip..].iter().position(|&c| c == b'\n').unwrap() + 1) as u64];
    for t in 1..threads {
        let at = (len * t / threads).max(cuts[0] + 1);
        cuts.push(boundary(&path, at, len).max(cuts[cuts.len() - 1]));
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
