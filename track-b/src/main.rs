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

/// A BOOL query is its (operator, term) steps, the first one `O`R-ed into the
/// empty set.
enum Query {
    Bool(Vec<(u8, u32)>),
    Phrase(u32),
}
#[derive(Default)]
struct Plan<'a> {
    terms: Map<&'a [u8], (u32, u32)>,
    short: Map<u128, (u32, u32)>,
    bools: u32,
    early: u32,
    pids: u32,
    phrases: Vec<Vec<u32>>,
    pairs: Map<u64, Vec<(u32, usize)>>,
    bloom: Vec<u64>,
    trig: Vec<u64>,
}

impl<'a> Plan<'a> {
    /// The (BOOL id, phrase id) entry of a term.
    fn slot(&mut self, w: &'a [u8]) -> &mut (u32, u32) {
        if w.len() <= 16 { self.short.entry(key(w)).or_insert((NO, NO)) } else { self.terms.entry(w).or_insert((NO, NO)) }
    }
}

/// A term of up to 16 bytes packed into a u128, lowercased (alphanumeric bytes
/// OR 0x20 are their lowercase).
fn key(w: &[u8]) -> u128 {
    w.iter().rev().fold(0, |k, &c| k << 8 | (c | 0x20) as u128)
}

fn pair(a: u32, b: u32) -> (u64, usize) {
    let k = (a as u64) << 32 | b as u64;
    (k, (k.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 44) as usize)
}

#[derive(Default)]
struct Slice {
    first: u32,
    w: usize,
    ent: Vec<[u32; 2]>,
    bits: Vec<u64>,
    ids: Vec<u16>,
    hoff: Vec<u32>,
    hits: Vec<u32>,
}

struct Scan<L> {
    log: Vec<L>,
    ends: Vec<u32>,
    st: Vec<u32>,
    early: Vec<u64>,
    found: Vec<(u32, u32)>,
    first: u32,
}

// Bitmap words per early term: a slice holds at most 65536 docs.
const EW: usize = 1024;

fn parse<'a>(input: &'a [u8], sample: &[u8]) -> (Vec<Query>, Plan<'a>) {
    let (mut p, mut queries, mut seen) = (Plan::default(), Vec::with_capacity(input.len() / 30), Map::default());
    p.short.reserve(input.len() / 20);
    let (mut bools, mut pids) = (0, 0);
    for line in input.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
        if let Some(rest) = line.strip_prefix(b"BOOL ") {
            let (mut steps, mut op) = (Vec::with_capacity(6), b'O');
            for (i, w) in rest.split(|&b| b == b' ').enumerate() {
                if i % 2 == 1 {
                    op = w[0];
                    continue;
                }
                let t = p.slot(w);
                if t.0 == NO {
                    (t.0, bools) = (bools, bools + 1);
                }
                steps.push((op, t.0));
            }
            queries.push(Query::Bool(steps));
        } else {
            // Equal phrase lines are equal term sequences: parse each text once.
            let n = p.phrases.len() as u32;
            let id = *seen.entry(line).or_insert(n);
            if id == n {
                let mut ph = Vec::with_capacity(8);
                for w in line[8..line.len() - 1].split(|&b| b == b' ') {
                    let t = p.slot(w);
                    if t.1 == NO {
                        (t.1, pids) = (pids, pids + 1);
                    }
                    ph.push(t.1);
                }
                p.phrases.push(ph);
            }
            queries.push(Query::Phrase(id));
        }
    }
    (p.bools, p.pids) = (bools, pids);
    // Phrase term frequencies in a corpus sample pick each phrase's rarest pair.
    let (mut freq, mut df, mut last, mut docs) = (vec![0u64; pids as usize], vec![0u32; bools as usize], vec![NO; bools as usize], 0);
    for (d, line) in sample.split(|&b| b == b'\n').enumerate() {
        docs = d + 1;
        for w in line.split(|b| !b.is_ascii_alphanumeric()).filter(|w| (1..=16).contains(&w.len())) {
            let Some(&(bid, pid)) = p.short.get(&key(w)) else { continue };
            if pid != NO {
                freq[pid as usize] += 1;
            }
            if bid != NO && last[bid as usize] != d as u32 {
                (last[bid as usize], df[bid as usize]) = (d as u32, df[bid as usize] + 1);
            }
        }
    }
    // Renumber BOOL ids by sample df: terms in 1/16 of the docs start as bitmaps,
    // and the scan's per-term state for the rest stays cache-friendly.
    let mut order: Vec<u32> = (0..bools).collect();
    order.sort_by_key(|&b| std::cmp::Reverse(df[b as usize]));
    p.early = order.iter().take_while(|&&b| df[b as usize] as usize * 16 >= docs).count() as u32;
    order.iter().enumerate().for_each(|(r, &b)| last[b as usize] = r as u32);
    let remap = |t: &mut (u32, u32)| t.0 = if t.0 == NO { NO } else { last[t.0 as usize] };
    p.short.values_mut().for_each(remap);
    p.terms.values_mut().for_each(remap);
    for q in &mut queries {
        if let Query::Bool(steps) = q {
            steps.iter_mut().for_each(|(_, b)| *b = last[*b as usize]);
        }
    }
    (p.bloom, p.trig) = (vec![0; 1 << 14], vec![0; (pids as usize).div_ceil(64)]);
    for (n, ph) in p.phrases.iter().enumerate() {
        let a = (0..ph.len() - 1).min_by_key(|&i| (freq[ph[i] as usize] + 1) * (freq[ph[i + 1] as usize] + 1)).unwrap();
        let (k, h) = pair(ph[a], ph[a + 1]);
        p.pairs.entry(k).or_default().push((n as u32, a));
        p.bloom[h / 64] |= 1 << (h % 64);
        let r = if freq[ph[a] as usize] <= freq[ph[a + 1] as usize] { ph[a] } else { ph[a + 1] };
        p.trig[r as usize / 64] |= 1 << (r % 64);
    }
    (queries, p)
}

/// Index the lines of `path` that start in `start..end`, streaming the file. Log
/// entries are `L`: u16 when every BOOL term id fits.
fn index_range<L: Copy + Default + TryFrom<u32> + Into<u32>>(path: &Path, start: u64, end: u64, p: &Plan) -> Slice {
    let n = p.bools as usize;
    // Untouched capacity costs no memory; this just avoids regrowth copies.
    let (log, ends) = (Vec::with_capacity((end - start) as usize / 8), Vec::with_capacity((end - start) as usize / 128));
    let early = vec![0; p.early as usize * EW];
    let mut sc = Scan::<L> { log, ends, st: vec![NO; n], early, found: Vec::new(), first: 0 };
    let mut f = File::open(path).unwrap();
    // Begin one byte early and drop the first (partial) line, so a line that
    // starts exactly at `start` is kept.
    let mut off = start.saturating_sub(1);
    f.seek(SeekFrom::Start(off)).unwrap();
    // 64 spare bytes past the data let the scanner read whole blocks.
    let (mut buf, mut have, mut skip) = (vec![0u8; (1 << 18) + 64], 0, start > 0);
    let (mut seq, mut bids) = (Vec::new(), Vec::new());
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
        if !index_lines(&mut sc, &mut seq, &mut bids, &buf[i..], last + 1 - i, off + i as u64, end, p) || n == 0 {
            break;
        }
        buf.copy_within(last + 1..have, 0);
        (off, have) = (off + last as u64 + 1, have - last - 1);
    }
    drop(buf);

    // Common terms become bitmaps, the rest CSR lists of local doc indexes.
    let nd = sc.ends.len();
    assert!(nd <= 1 << 16, "slice too large for u16 doc indexes");
    let mut sl = Slice { first: sc.first, w: nd.div_ceil(64), ent: vec![[0, 0]; n], ..Default::default() };
    for &b in &sc.log {
        sl.ent[b.into() as usize][1] += 1;
    }
    // Early terms come first, so term t < `p.early` gets bitmap t. A list's end
    // serves as its write cursor while it fills.
    let (mut dense, mut at) = (0, 0);
    for (t, e) in sl.ent.iter_mut().enumerate() {
        if t < p.early as usize || e[1] as usize * 32 >= nd.max(1) {
            (*e, dense) = ([dense, NO], dense + 1);
        } else {
            (*e, at) = ([at, at], at + e[1]);
        }
    }
    (sl.bits, sl.ids) = (vec![0; dense as usize * sl.w], vec![0; at as usize]);
    for t in 0..p.early as usize {
        sl.bits[t * sl.w..][..sl.w].copy_from_slice(&sc.early[t * EW..][..sl.w]);
    }
    let mut s = 0;
    for (li, &e) in sc.ends.iter().enumerate() {
        for &b in &sc.log[s..e as usize] {
            let x = &mut sl.ent[b.into() as usize];
            if x[1] == NO {
                sl.bits[x[0] as usize * sl.w + li / 64] |= 1 << (li % 64);
            } else {
                (sl.ids[x[1] as usize], x[1]) = (li as u16, x[1] + 1);
            }
        }
        s = e as usize;
    }
    // Phrase matches, grouped by phrase.
    sc.found.sort_unstable();
    sc.found.dedup();
    sl.hoff = vec![0; p.phrases.len() + 1];
    sc.found.iter().for_each(|&(q, _)| sl.hoff[q as usize + 1] += 1);
    (1..sl.hoff.len()).for_each(|q| sl.hoff[q] += sl.hoff[q - 1]);
    sl.hits = sc.found.iter().map(|f| f.1).collect();
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
fn index_lines<L: Copy + Default + TryFrom<u32> + Into<u32>>(
    sc: &mut Scan<L>,
    seq: &mut Vec<u32>,
    bids: &mut Vec<u32>,
    text: &[u8],
    len: usize,
    base: u64,
    end: u64,
    p: &Plan,
) -> bool {
    let (mut doc, mut start, mut in_tok, mut id_next) = (0, 0, false, true);
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
                    if sc.ends.is_empty() {
                        sc.first = doc;
                    }
                    id_next = false;
                    seq.clear();
                    bids.clear();
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
                let &(bid, pid) = t.unwrap_or(&(NO, NO));
                bids.push(bid);
                seq.push(pid);
            }
            if nl >> q & 1 == 1 {
                // The doc's terms, in tight loops apart from the lexer.
                let li = sc.ends.len();
                for &bid in bids.iter() {
                    // Setting a bit twice is harmless, so early terms skip the dedupe.
                    if bid < p.early {
                        sc.early[bid as usize * EW + li / 64] |= 1 << (li % 64);
                    } else if bid != NO && sc.st[bid as usize] != doc {
                        sc.st[bid as usize] = doc;
                        sc.log.push(L::try_from(bid).unwrap_or_default());
                    }
                }
                sc.ends.push(sc.log.len() as u32);
                for (j, &t) in seq.iter().enumerate() {
                    if t == NO || p.trig[t as usize / 64] >> (t % 64) & 1 == 0 {
                        continue;
                    }
                    // The pairs (i, i + 1) that hold position j.
                    for i in j.max(1) - 1..(j + 1).min(seq.len() - 1) {
                        let (k, h) = pair(seq[i], seq[i + 1]);
                        if p.bloom[h / 64] >> (h % 64) & 1 == 0 {
                            continue;
                        }
                        for &(ph, a) in p.pairs.get(&k).into_iter().flatten() {
                            if i >= a && seq[i - a..].starts_with(&p.phrases[ph as usize]) {
                                sc.found.push((ph, doc));
                            }
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

/// Evaluate a BOOL query on one slice. The result is the sorted local doc
/// indexes in `l` while only sparse terms feed it, else bitmap `r`; returns
/// whether `r` holds it.
fn run(s: &Slice, steps: &[(u8, u32)], l: &mut Vec<u16>, r: &mut Vec<u64>) -> bool {
    let (mut bits, w) = (false, s.w);
    l.clear();
    for &(op, t) in steps {
        let [a, b] = s.ent[t as usize];
        let (k, list) = if b == NO { (a as usize, &[][..]) } else { (NO as usize, &s.ids[a as usize..b as usize]) };
        let has = |d: u16| s.bits[k * w + d as usize / 64] >> (d % 64) & 1 == 1;
        let inr = |r: &[u64], d: u16| r[d as usize / 64] >> (d % 64) & 1 == 1;
        match (bits, op, k != NO as usize) {
            (false, b'O', true) => {
                r.clear();
                r.extend_from_slice(&s.bits[k * w..(k + 1) * w]);
                l.iter().for_each(|&d| r[d as usize / 64] |= 1 << (d % 64));
                bits = true;
            }
            (false, b'O', false) if l.is_empty() => l.extend_from_slice(list),
            (false, b'O', false) => {
                // Two sorted runs: the stable sort merges them in one pass.
                l.extend_from_slice(list);
                l.sort();
                l.dedup();
            }
            (false, b'A', true) => l.retain(|&d| has(d)),
            // Intersect by searching the longer list for each id of the shorter.
            (false, b'A', false) if list.len() < l.len() => {
                let v: Vec<u16> = list.iter().copied().filter(|d| l.binary_search(d).is_ok()).collect();
                *l = v;
            }
            (false, b'A', false) => l.retain(|d| list.binary_search(d).is_ok()),
            (false, _, true) => l.retain(|&d| !has(d)),
            (false, _, false) => l.retain(|d| list.binary_search(d).is_err()),
            (true, b'A', false) => {
                l.clear();
                l.extend(list.iter().filter(|&&d| inr(r, d)));
                bits = false;
            }
            (true, b'O', false) => list.iter().for_each(|&d| r[d as usize / 64] |= 1 << (d % 64)),
            (true, _, false) => list.iter().for_each(|&d| r[d as usize / 64] &= !(1 << (d % 64))),
            (true, op, true) => {
                let b = &s.bits[k * w..(k + 1) * w];
                match op {
                    b'A' => r.iter_mut().zip(b).for_each(|(x, y)| *x &= y),
                    b'O' => r.iter_mut().zip(b).for_each(|(x, y)| *x |= y),
                    _ => r.iter_mut().zip(b).for_each(|(x, y)| *x &= !y),
                }
            }
        }
    }
    bits
}

fn prefetch<T>(x: &T) {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: a prefetch is only a hint; `x` is a valid reference.
    unsafe {
        use std::arch::x86_64::*;
        _mm_prefetch::<_MM_HINT_T0>(x as *const T as *const i8);
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

/// Run `f(i)` for every `i < n` on `threads` threads; results in order of `i`.
fn par<T: Send>(n: usize, threads: usize, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
    let next = AtomicUsize::new(0);
    let mut out: Vec<(usize, T)> = thread::scope(|s| {
        let hs: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= n {
                            return mine;
                        }
                        mine.push((i, f(i)));
                    }
                })
            })
            .collect();
        hs.into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    out.sort_unstable_by_key(|o| o.0);
    out.into_iter().map(|o| o.1).collect()
}

fn main() {
    let dir = |var, default| std::env::var_os(var).map_or(default, |v| v.into_string().unwrap());
    let data = dir("SPEED_DEMON_DATA_DIR", "/data".into());
    let out_dir = dir("SPEED_DEMON_OUTPUT_DIR", "/out".into());
    let threads = thread::available_parallelism().map_or(12, |n| n.get());
    let qtext = std::fs::read(Path::new(&data).join("queries.txt")).unwrap();
    let path = Path::new(&data).join("docs.txt");
    let mut sample = vec![0; 1 << 18];
    let got = File::open(&path).unwrap().read(&mut sample).unwrap();
    let (qs, plan) = parse(&qtext, &sample[..got]);

    // One slice per thread (each query visits every slice, so more cost query
    // time); at least 8 keep each one's doc count within u16 local indexes.
    let len = std::fs::metadata(&path).unwrap().len();
    let ns = threads.max(8) as u64;
    let ix = par(ns as usize, threads, |i| {
        let (a, b) = (len * i as u64 / ns, len * (i as u64 + 1) / ns);
        if plan.bools <= 1 << 16 { index_range::<u16>(&path, a, b, &plan) } else { index_range::<u32>(&path, a, b, &plan) }
    });

    // Threads answer blocks of queries slice by slice, so each slice's index
    // stays cache-hot across the block. Four blocks per thread keep the threads
    // finishing together.
    let qb = qs.len().div_ceil(threads * 4).max(1);
    let blocks = par(qs.len().div_ceil(qb), threads, |b| {
        let (mut buf, mut r, mut l) = (Vec::new(), Vec::new(), Vec::new());
        let block = &qs[b * qb..(b * qb + qb).min(qs.len())];
        let mut acc = vec![(0, 0u64, Vec::<u64>::new()); block.len()];
        for s in &ix {
            for (i, (q, (count, sum, first))) in block.iter().zip(&mut acc).enumerate() {
                // Lookups are cache misses: fetch the entries 8 queries ahead and
                // the lists or bitmaps they point to 4 ahead.
                if let Some(Query::Bool(steps)) = block.get(i + 8) {
                    steps.iter().for_each(|&(_, t)| prefetch(&s.ent[t as usize]));
                }
                if let Some(Query::Bool(steps)) = block.get(i + 4) {
                    for &(_, t) in steps {
                        let [a, b] = s.ent[t as usize];
                        if b == NO {
                            prefetch(&s.bits[a as usize * s.w]);
                        } else if a < b {
                            prefetch(&s.ids[a as usize]);
                        }
                    }
                }
                match q {
                    Query::Phrase(p) => {
                        let ids = &s.hits[s.hoff[*p as usize] as usize..s.hoff[*p as usize + 1] as usize];
                        *count += ids.len() as u64;
                        *sum += ids.iter().map(|&x| x as u64).sum::<u64>();
                        first.extend(ids.iter().take(10 - first.len()).map(|&x| x as u64));
                    }
                    Query::Bool(steps) if !run(s, steps, &mut l, &mut r) => {
                        *count += l.len() as u64;
                        *sum += l.iter().map(|&d| (s.first + d as u32) as u64).sum::<u64>();
                        first.extend(l.iter().take(10 - first.len()).map(|&d| (s.first + d as u32) as u64));
                    }
                    Query::Bool(..) => {
                        for (i, &x) in r.iter().enumerate().filter(|w| *w.1 != 0) {
                            // Sum of set bit positions, one popcount per index bit.
                            let (base, c) = (s.first as u64 + 64 * i as u64, x.count_ones() as u64);
                            let pos: u64 = MASKS.iter().enumerate().map(|(k, m)| ((x & m).count_ones() as u64) << k).sum();
                            (*count, *sum) = (*count + c, *sum + base * c + pos);
                            let mut y = x;
                            while first.len() < 10 && y != 0 {
                                first.push(base + y.trailing_zeros() as u64);
                                y &= y - 1;
                            }
                        }
                    }
                }
            }
        }
        for (count, sum, first) in acc {
            write!(buf, "{count} {sum}").unwrap();
            first.iter().for_each(|x| write!(buf, " {x}").unwrap());
            buf.push(b'\n');
        }
        buf
    });
    std::fs::create_dir_all(&out_dir).unwrap();
    std::fs::write(Path::new(&out_dir).join("results.txt"), blocks.concat()).unwrap();
    // Skip freeing the index; the OS reclaims it.
    std::process::exit(0);
}
