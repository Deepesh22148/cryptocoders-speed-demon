use std::collections::HashMap;
use std::fs::File;
use std::hash::{BuildHasherDefault, Hasher};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

// Fast hasher: keys are short, SipHash is overkill.
#[derive(Default)]
struct Fx(u64);

impl Hasher for Fx {
    fn write(&mut self, bytes: &[u8]) {
        for c in bytes.chunks(8) {
            let mut w = [0u8; 8];
            w[..c.len()].copy_from_slice(c);
            self.write_u64(u64::from_le_bytes(w));
        }
    }
    // Folded multiply: every input bit reaches the low bits the table indexes by.
    fn write_u64(&mut self, w: u64) {
        let x = (self.0 ^ w) as u128 * 0x9e37_79b9_7f4a_7c15;
        self.0 = x as u64 ^ (x >> 64) as u64;
    }
    fn write_u128(&mut self, w: u128) {
        self.write_u64(w as u64 ^ (w >> 64) as u64);
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

type Map<K, V> = HashMap<K, V, BuildHasherDefault<Fx>>;
const NO: u32 = u32::MAX;
// A phrase id's top bits flag terms that open (bit 31) or close (bit 30) some
// phrase's anchor pair; `PID` masks them off and is the id of non-phrase tokens.
const PID: u32 = (1 << 30) - 1;
// Docs per slice scanned before its common terms switch to direct bitmaps.
const PROBE: usize = 1024;

enum Query {
    Bool(Vec<u32>, Vec<u8>),
    Phrase(u32),
}

/// Everything known from the queries before the corpus is read. A term maps to
/// (BOOL posting id, phrase term id); terms up to 16 bytes are also keyed by their
/// bytes packed into a u128 (`short`), which the scan builds as it lexes. Phrases
/// are pid sequences found during the scan through `pairs`: each phrase is keyed
/// by one adjacent pid pair (its longest words, likely the rarest under Zipf),
/// mapping to (phrase id, pair offset). `bloom` is a bit filter over those pairs.
#[derive(Default)]
struct Plan<'a> {
    terms: Map<&'a [u8], (u32, u32)>,
    short: Map<u128, (u32, u32)>,
    bools: u32,
    pids: u32,
    phrases: Vec<Vec<u32>>,
    pairs: Map<u64, Vec<(u32, usize)>>,
    bloom: Vec<u64>,
}

fn pair(a: u32, b: u32) -> (u64, usize) {
    let k = (a as u64) << 32 | b as u64;
    (k, (k.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 44) as usize)
}

/// Results for one byte range of the corpus, which holds docs `first..` (ranges
/// are disjoint and ascending, so they are never merged). A common BOOL term has
/// bitmap `bm[t]` (`w` words in `bits`) over local doc indexes; any other term
/// has the sorted local indexes `ids[off[t]..off[t + 1]]`. Phrases have sorted
/// doc ids. During the scan each doc's new BOOL term ids are appended to `log`,
/// ending at `docs[i].1`, except for terms already common in the first `PROBE`
/// docs, which set bits in `early[dk[t]]` directly.
#[derive(Default)]
struct Slice {
    log: Vec<u32>,
    docs: Vec<(u32, u32)>,
    last: Vec<u32>,
    dk: Vec<u32>,
    early: Vec<Vec<u64>>,
    first: u32,
    w: usize,
    bm: Vec<u32>,
    bits: Vec<u64>,
    off: Vec<u32>,
    ids: Vec<u32>,
    phr: Vec<Vec<u32>>,
}

fn parse(input: &[u8]) -> (Vec<Query>, Plan<'_>) {
    let (mut p, mut queries, mut seen) = (Plan::default(), Vec::new(), Map::default());
    for line in input.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
        if let Some(rest) = line.strip_prefix(b"BOOL ") {
            let words: Vec<&[u8]> = rest.split(|&b| b == b' ').collect();
            let mut ids = Vec::new();
            for w in words.iter().step_by(2) {
                let t = p.terms.entry(*w).or_insert((NO, NO));
                if t.0 == NO {
                    (t.0, p.bools) = (p.bools, p.bools + 1);
                }
                ids.push(t.0);
            }
            queries.push(Query::Bool(ids, words.iter().skip(1).step_by(2).map(|w| w[0]).collect()));
        } else {
            let (mut ph, words) = (Vec::new(), line[8..line.len() - 1].split(|&b| b == b' '));
            let lens: Vec<usize> = words.clone().map(|w| w.len()).collect();
            for w in words {
                let t = p.terms.entry(w).or_insert((NO, NO));
                if t.1 == NO {
                    (t.1, p.pids) = (p.pids, p.pids + 1);
                }
                ph.push(t.1);
            }
            let n = p.phrases.len() as u32;
            let id = *seen.entry(ph.clone()).or_insert(n);
            if id == n {
                let a = (0..ph.len() - 1).max_by_key(|&i| lens[i] + lens[i + 1]).unwrap();
                p.pairs.entry(pair(ph[a], ph[a + 1]).0).or_default().push((n, a));
                p.phrases.push(ph);
            }
            queries.push(Query::Phrase(id));
        }
    }
    p.bloom = vec![0; 1 << 14];
    let mut flags = vec![0u32; p.pids as usize];
    for k in p.pairs.keys() {
        let h = (k.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 44) as usize;
        p.bloom[h / 64] |= 1 << (h % 64);
        flags[(k >> 32) as usize] |= 1 << 31;
        flags[(*k as u32) as usize] |= 1 << 30;
    }
    for v in p.terms.values_mut() {
        v.1 = if v.1 == NO { PID } else { v.1 | flags[v.1 as usize] };
    }
    for (w, &v) in p.terms.iter().filter(|(w, _)| w.len() <= 16) {
        p.short.insert(w.iter().rev().fold(0, |k, &c| k << 8 | c as u128), v);
    }
    (queries, p)
}

/// Index the lines of `path` that start in `start..end`, streaming the file.
fn index_range(path: &Path, start: u64, end: u64, p: &Plan) -> Slice {
    let n = p.bools as usize;
    let mut sl = Slice { phr: vec![Vec::new(); p.phrases.len()], last: vec![NO; n], dk: vec![NO; n], ..Default::default() };
    // Untouched capacity costs no memory; this just avoids regrowth copies.
    (sl.log, sl.docs) = (Vec::with_capacity((end - start) as usize / 8), Vec::with_capacity((end - start) as usize / 128));
    let mut f = File::open(path).unwrap();
    // Begin one byte early and drop the first (partial) line, so a line that
    // starts exactly at `start` is kept.
    let mut off = start.saturating_sub(1);
    f.seek(SeekFrom::Start(off)).unwrap();
    // 64 spare bytes past the data let the scanner read whole blocks.
    let (mut buf, mut have, mut skip) = (vec![0u8; (1 << 20) + 64], 0, start > 0);
    let (mut seq, mut cand) = (Vec::new(), Vec::new());
    loop {
        if have + 64 == buf.len() {
            buf.resize(have * 2 + 64, 0);
        }
        let cap = buf.len() - 64;
        let n = f.read(&mut buf[have..cap]).unwrap();
        have += n;
        // Whole lines only; the tail waits for the next read.
        let Some(last) = buf[..have].iter().rposition(|&b| b == b'\n') else {
            if n == 0 {
                break;
            }
            continue;
        };
        let i = if skip { buf.iter().position(|&b| b == b'\n').unwrap() + 1 } else { 0 };
        skip = false;
        let more = index_lines(&mut sl, &mut seq, &mut cand, &buf[i..], last + 1 - i, off + i as u64, end, p);
        if !more || n == 0 {
            break;
        }
        buf.copy_within(last + 1..have, 0);
        (off, have) = (off + last as u64 + 1, have - last - 1);
    }
    let (nd, mut cnt) = (sl.docs.len(), vec![0u32; n]);
    for &b in &sl.log {
        cnt[b as usize] += 1;
    }
    (sl.first, sl.w) = (sl.docs.first().map_or(0, |d| d.0), nd.div_ceil(64));
    let mut dense = 0;
    sl.bm = (0..n).map(|t| if sl.dk[t] != NO || cnt[t] as usize * 32 >= nd.max(1) { (dense, dense += 1).0 } else { NO }).collect();
    sl.off = vec![0; n + 1];
    for t in 0..n {
        sl.off[t + 1] = sl.off[t] + if sl.bm[t] == NO { cnt[t] } else { 0 };
    }
    (sl.bits, sl.ids) = (vec![0; dense as usize * sl.w], vec![0; sl.off[n] as usize]);
    for (t, &k) in sl.dk.iter().enumerate().filter(|d| *d.1 != NO) {
        let e = &sl.early[k as usize];
        sl.bits[sl.bm[t] as usize * sl.w..][..e.len()].copy_from_slice(e);
    }
    let (mut pos, mut s) = (sl.off.clone(), 0);
    for (li, &(_, e)) in sl.docs.iter().enumerate() {
        for &b in &sl.log[s..e as usize] {
            let (b, k) = (b as usize, sl.bm[b as usize] as usize);
            if k != NO as usize {
                sl.bits[k * sl.w + li / 64] |= 1 << (li % 64);
            } else {
                (sl.ids[pos[b] as usize], pos[b]) = (li as u32, pos[b] + 1);
            }
        }
        s = e as usize;
    }
    (sl.log, sl.docs, sl.last, sl.early) = Default::default();
    sl
}

/// Bit masks of the alphanumeric bytes and the newlines in `b[..64]`.
#[cfg(target_arch = "x86_64")]
fn masks(b: &[u8]) -> (u64, u64) {
    use std::arch::x86_64::*;
    let (mut m, mut nl) = (0u64, 0u64);
    for k in 0..4 {
        // SAFETY: SSE2 is baseline on x86_64; the slice holds these 16 bytes.
        unsafe {
            let v = _mm_loadu_si128(b[16 * k..16 * k + 16].as_ptr() as *const __m128i);
            // Signed compares: bytes >= 0x80 are negative, so never alphanumeric.
            let c = _mm_or_si128(v, _mm_set1_epi8(0x20));
            let az = _mm_and_si128(_mm_cmpgt_epi8(c, _mm_set1_epi8(0x60)), _mm_cmplt_epi8(c, _mm_set1_epi8(0x7b)));
            let dg = _mm_and_si128(_mm_cmpgt_epi8(v, _mm_set1_epi8(0x2f)), _mm_cmplt_epi8(v, _mm_set1_epi8(0x3a)));
            m |= (_mm_movemask_epi8(_mm_or_si128(az, dg)) as u16 as u64) << (16 * k);
            nl |= (_mm_movemask_epi8(_mm_cmpeq_epi8(v, _mm_set1_epi8(10))) as u16 as u64) << (16 * k);
        }
    }
    (m, nl)
}

#[cfg(not(target_arch = "x86_64"))]
fn masks(b: &[u8]) -> (u64, u64) {
    b[..64].iter().enumerate().fold((0, 0), |(m, nl), (k, &c)| {
        (m | (((c | 0x20).is_ascii_lowercase() || c.is_ascii_digit()) as u64) << k, nl | ((c == 10) as u64) << k)
    })
}

/// Index the whole lines in `text[..len]` (ending with a newline; 64 readable
/// bytes follow) whose file offset (`base` + index) is before `end`. A line's
/// first token is its doc id. Returns false once a line starts at `end`.
fn index_lines(sl: &mut Slice, seq: &mut Vec<u32>, cand: &mut Vec<usize>, text: &[u8], len: usize, base: u64, end: u64, p: &Plan) -> bool {
    let (mut doc, mut start, mut in_tok, mut id_next, mut prevf) = (0, 0, false, true, 0u32);
    if base >= end {
        return false;
    }
    for at in (0..len).step_by(64) {
        let valid = if len - at >= 64 { !0 } else { (1u64 << (len - at)) - 1 };
        let (m, nl) = masks(&text[at..at + 64]);
        let (m, nl) = (m & valid, nl & valid);
        // Bits where a token starts or ends.
        let flips = m ^ (m << 1 | in_tok as u64);
        let mut ev = flips | nl;
        while ev != 0 {
            let q = ev.trailing_zeros() as usize;
            ev &= ev - 1;
            if flips >> q & 1 == 1 && !in_tok {
                (start, in_tok) = (at + q, true);
            } else if flips >> q & 1 == 1 {
                in_tok = false;
                let (s, e) = (start, at + q);
                if id_next {
                    doc = text[s..e].iter().fold(0, |a, &b| a * 10 + (b - b'0') as u32);
                    id_next = false;
                    seq.clear();
                    cand.clear();
                    prevf = 0;
                    continue;
                }
                // Alphanumeric bytes OR 0x20 are their lowercase; tokens over 32
                // bytes never equal a query term.
                let t = match e - s {
                    l @ ..=16 => {
                        let raw = u128::from_le_bytes(text[s..s + 16].try_into().unwrap());
                        p.short.get(&((raw | 0x2020_2020_2020_2020_2020_2020_2020_2020) & (!0 >> (128 - 8 * l))))
                    }
                    ..=32 => p.terms.get(&text[s..e].iter().map(|b| b | 0x20).collect::<Vec<u8>>()[..]),
                    _ => None,
                };
                let &(bid, pid) = t.unwrap_or(&(NO, PID));
                if bid != NO {
                    let (k, li) = (sl.dk[bid as usize] as usize, sl.docs.len());
                    if k != NO as usize {
                        let v = &mut sl.early[k];
                        if v.len() <= li / 64 {
                            v.resize(li / 64 + 1, 0);
                        }
                        v[li / 64] |= 1 << (li % 64);
                    } else if sl.last[bid as usize] != doc {
                        sl.last[bid as usize] = doc;
                        sl.log.push(bid);
                    }
                }
                if prevf >> 31 & pid >> 30 & 1 == 1 {
                    let h = pair(seq[seq.len() - 1], pid & PID).1;
                    if p.bloom[h / 64] >> (h % 64) & 1 != 0 {
                        cand.push(seq.len());
                    }
                }
                prevf = pid;
                seq.push(pid & PID);
            }
            if nl >> q & 1 == 1 {
                sl.docs.push((doc, sl.log.len() as u32));
                if sl.docs.len() == PROBE {
                    // Terms already in 1/16 of the docs go straight to bitmaps.
                    let mut cnt = vec![0u32; sl.last.len()];
                    sl.log.iter().for_each(|&b| cnt[b as usize] += 1);
                    for (t, _) in cnt.iter().enumerate().filter(|c| *c.1 as usize * 16 >= PROBE) {
                        sl.dk[t] = sl.early.len() as u32;
                        sl.early.push(Vec::new());
                    }
                }
                for &j in cand.iter() {
                    for &(ph, a) in p.pairs.get(&pair(seq[j - 1], seq[j]).0).into_iter().flatten() {
                        let v = &mut sl.phr[ph as usize];
                        if j > a && seq[j - 1 - a..].starts_with(&p.phrases[ph as usize]) && v.last() != Some(&doc) {
                            v.push(doc);
                        }
                    }
                }
                id_next = true;
                if base + (at + q + 1) as u64 >= end {
                    return false;
                }
            }
        }
    }
    true
}

/// Evaluate a BOOL query on one slice into `r`, a bitmap of local doc indexes.
fn run(s: &Slice, terms: &[u32], ops: &[u8], r: &mut Vec<u64>, tmp: &mut Vec<u64>) {
    r.clear();
    r.resize(s.w, 0);
    for (i, &t) in terms.iter().enumerate() {
        let (op, k) = (if i == 0 { b'O' } else { ops[i - 1] }, s.bm[t as usize] as usize);
        if k != NO as usize {
            let b = &s.bits[k * s.w..(k + 1) * s.w];
            match op {
                b'A' => r.iter_mut().zip(b).for_each(|(x, y)| *x &= y),
                b'O' => r.iter_mut().zip(b).for_each(|(x, y)| *x |= y),
                _ => r.iter_mut().zip(b).for_each(|(x, y)| *x &= !y),
            }
            continue;
        }
        let list = &s.ids[s.off[t as usize] as usize..s.off[t as usize + 1] as usize];
        match op {
            b'A' => {
                tmp.clear();
                tmp.resize(s.w, 0);
                for &d in list {
                    tmp[d as usize / 64] |= r[d as usize / 64] & 1 << (d % 64);
                }
                std::mem::swap(r, tmp);
            }
            b'O' => list.iter().for_each(|&d| r[d as usize / 64] |= 1 << (d % 64)),
            _ => list.iter().for_each(|&d| r[d as usize / 64] &= !(1 << (d % 64))),
        }
    }
}

const MASKS: [u64; 6] = [
    0xaaaa_aaaa_aaaa_aaaa,
    0xcccc_cccc_cccc_cccc,
    0xf0f0_f0f0_f0f0_f0f0,
    0xff00_ff00_ff00_ff00,
    0xffff_0000_ffff_0000,
    0xffff_ffff_0000_0000,
];

fn main() {
    let dir = |var, default| std::env::var_os(var).map_or(default, |v| v.into_string().unwrap());
    let data = dir("SPEED_DEMON_DATA_DIR", "/data".into());
    let out_dir = dir("SPEED_DEMON_OUTPUT_DIR", "/out".into());
    let threads = thread::available_parallelism().map_or(12, |n| n.get());
    let qtext = std::fs::read(Path::new(&data).join("queries.txt")).unwrap();
    let (qs, plan) = parse(&qtext);

    // One slice per thread over its own byte range of the corpus.
    let path = Path::new(&data).join("docs.txt");
    let len = std::fs::metadata(&path).unwrap().len();
    let t = threads as u64;
    let ix: Vec<Slice> = thread::scope(|s| {
        let hs: Vec<_> = (0..t)
            .map(|i| {
                let (path, plan) = (&path, &plan);
                s.spawn(move || index_range(path, len * i / t, len * (i + 1) / t, plan))
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });

    // Threads take blocks of 64 queries; blocks are stitched back in order.
    let next = AtomicUsize::new(0);
    let mut done: Vec<(usize, Vec<u8>)> = thread::scope(|s| {
        let hs: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let b = next.fetch_add(1, Ordering::Relaxed);
                        if b * 64 >= qs.len() {
                            return mine;
                        }
                        let (mut buf, mut r, mut tmp) = (Vec::new(), Vec::new(), Vec::new());
                        for q in &qs[b * 64..(b * 64 + 64).min(qs.len())] {
                            let (mut count, mut sum, mut first) = (0, 0u64, Vec::<u64>::new());
                            for s in &ix {
                                match q {
                                    Query::Phrase(p) => {
                                        let ids = &s.phr[*p as usize];
                                        count += ids.len() as u64;
                                        sum += ids.iter().map(|&x| x as u64).sum::<u64>();
                                        first.extend(ids.iter().take(10 - first.len()).map(|&x| x as u64));
                                    }
                                    Query::Bool(terms, ops) => {
                                        run(s, terms, ops, &mut r, &mut tmp);
                                        for (i, &x) in r.iter().enumerate().filter(|w| *w.1 != 0) {
                                            // Sum of set bit positions, one popcount per index bit.
                                            let (base, c) = (s.first as u64 + 64 * i as u64, x.count_ones() as u64);
                                            let pos: u64 = MASKS.iter().enumerate().map(|(k, m)| ((x & m).count_ones() as u64) << k).sum();
                                            (count, sum) = (count + c, sum + base * c + pos);
                                            let mut y = x;
                                            while first.len() < 10 && y != 0 {
                                                first.push(base + y.trailing_zeros() as u64);
                                                y &= y - 1;
                                            }
                                        }
                                    }
                                }
                            }
                            write!(buf, "{count} {sum}").unwrap();
                            first.iter().for_each(|x| write!(buf, " {x}").unwrap());
                            buf.push(b'\n');
                        }
                        mine.push((b, buf));
                    }
                })
            })
            .collect();
        hs.into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    done.sort_unstable_by_key(|d| d.0);
    std::fs::create_dir_all(&out_dir).unwrap();
    let out: Vec<u8> = done.into_iter().flat_map(|d| d.1).collect();
    std::fs::write(Path::new(&out_dir).join("results.txt"), out).unwrap();
    // Skip freeing the index; the OS reclaims it.
    std::process::exit(0);
}
