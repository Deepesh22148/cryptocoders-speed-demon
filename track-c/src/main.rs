use std::collections::HashMap;
use std::fs::File;
use std::hash::{BuildHasherDefault, Hasher};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::thread;

// Fx-style hasher: keys are short, SipHash is overkill.
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
    fn write_u64(&mut self, w: u64) {
        self.0 = (self.0.rotate_left(5) ^ w).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

type FxMap<K, V> = HashMap<K, V, BuildHasherDefault<Fx>>;

const BOM: &[u8] = b"\xEF\xBB\xBF";
const HEADER: &[u8] = b"id,name,note,region_id,amount";
const CHUNK: usize = 1 << 20;

#[derive(Clone)]
struct Agg {
    count: u64,
    id_sum: u64,
    sum: i64,
    min: i64,
    max: i64,
    name_bytes: u64,
    note_bytes: u64,
}

impl Agg {
    fn new() -> Agg {
        Agg { count: 0, id_sum: 0, sum: 0, min: i64::MAX, max: i64::MIN, name_bytes: 0, note_bytes: 0 }
    }
}

// Per-thread result: aggregates per group, its own name ids, and which names each group saw.
struct Part {
    aggs: Vec<Agg>,
    names: FxMap<Vec<u8>, u32>,
    seen: Vec<Vec<u64>>,
}

// Every parser below returns None when the buffer runs out mid-record, so the caller reads more.
fn bad(p: usize) -> ! {
    panic!("malformed records.csv near buffer offset {p}")
}

fn digits(b: &[u8], p: &mut usize) -> Option<u64> {
    let s = *p;
    let mut v = 0u64;
    loop {
        let c = *b.get(*p)?;
        if !c.is_ascii_digit() {
            if *p == s {
                bad(s)
            }
            return Some(v);
        }
        v = v * 10 + (c - b'0') as u64;
        *p += 1;
    }
}

fn expect(b: &[u8], p: &mut usize, c: u8) -> Option<()> {
    if *b.get(*p)? != c {
        bad(*p)
    }
    *p += 1;
    Some(())
}

// A name or note: (start, end, number of "" escapes). The raw quoted bytes are a
// one-to-one stand-in for the unescaped value, so they serve as the distinct key.
fn text(b: &[u8], p: &mut usize) -> Option<(usize, usize, usize)> {
    if *b.get(*p)? != b'"' {
        let s = *p;
        *p += b[s..].iter().position(|&c| matches!(c, b',' | b'"' | b'\r' | b'\n'))?;
        return Some((s, *p, 0));
    }
    let s = *p + 1;
    let mut q = s;
    let mut esc = 0;
    loop {
        q += b[q..].iter().position(|&c| c == b'"')?;
        if *b.get(q + 1)? == b'"' {
            esc += 1;
            q += 2;
        } else {
            *p = q + 1;
            return Some((s, q, esc));
        }
    }
}

fn cents(b: &[u8], p: &mut usize) -> Option<i64> {
    let neg = *b.get(*p)? == b'-';
    *p += neg as usize;
    let whole = digits(b, p)? as i64;
    expect(b, p, b'.')?;
    let f = b.get(*p..*p + 2)?;
    if !f.iter().all(u8::is_ascii_digit) {
        bad(*p)
    }
    *p += 2;
    let v = whole * 100 + ((f[0] - b'0') * 10 + (f[1] - b'0')) as i64;
    Some(if neg { -v } else { v })
}

impl Part {
    // Parses one record at p into the aggregates; returns the offset after its terminator.
    fn record(&mut self, b: &[u8], p: usize, region: &[u16], unmatched: usize) -> Option<usize> {
        let mut p = p;
        if *b.get(p)? == BOM[0] {
            if b.get(p..p + 3)? != BOM {
                bad(p)
            }
            p += 3;
        }
        let id = digits(b, &mut p)?;
        expect(b, &mut p, b',')?;
        let (ns, ne, nesc) = text(b, &mut p)?;
        expect(b, &mut p, b',')?;
        let (ts, te, tesc) = text(b, &mut p)?;
        expect(b, &mut p, b',')?;
        let rid = digits(b, &mut p)? as usize;
        expect(b, &mut p, b',')?;
        let v = cents(b, &mut p)?;
        if *b.get(p)? == b'\r' {
            p += 1;
        }
        expect(b, &mut p, b'\n')?;

        let g = match region.get(rid) {
            Some(&c) if c != u16::MAX => c as usize,
            _ => unmatched,
        };
        let a = &mut self.aggs[g];
        a.count += 1;
        a.id_sum += id;
        a.sum += v;
        a.min = a.min.min(v);
        a.max = a.max.max(v);
        a.name_bytes += (ne - ns - nesc) as u64;
        a.note_bytes += (te - ts - tesc) as u64;

        let name = &b[ns..ne];
        let n = match self.names.get(name) {
            Some(&n) => n,
            None => {
                let n = self.names.len() as u32;
                self.names.insert(name.to_vec(), n);
                n
            }
        } as usize;
        let s = &mut self.seen[g];
        if s.len() <= n / 64 {
            s.resize(n / 64 + 1, 0);
        }
        s[n / 64] |= 1 << (n % 64);
        Some(p)
    }
}

// Streams records starting in [start, end) in 1 MB reads; the last one may run past end.
fn scan(path: &Path, start: u64, end: u64, region: &[u16], groups: usize) -> Part {
    let mut part = Part {
        aggs: vec![Agg::new(); groups],
        names: FxMap::default(),
        seen: vec![Vec::new(); groups],
    };
    let mut f = File::open(path).unwrap();
    f.seek(SeekFrom::Start(start)).unwrap();
    let mut buf: Vec<u8> = Vec::with_capacity(2 * CHUNK);
    let mut base = start; // file offset of buf[0]
    let mut pos = 0;
    let mut eof = false;
    while base + (pos as u64) < end {
        match part.record(&buf, pos, region, groups - 1) {
            Some(p) => pos = p,
            None => {
                if eof {
                    bad(pos)
                }
                buf.drain(..pos);
                base += pos as u64;
                pos = 0;
                let n = buf.len();
                buf.resize(n + CHUNK, 0);
                let got = f.read(&mut buf[n..]).unwrap();
                buf.truncate(n + got);
                if got == 0 {
                    // The final record may lack its terminator.
                    eof = true;
                    if buf.last() != Some(&b'\n') {
                        buf.push(b'\n');
                    }
                }
            }
        }
    }
    part
}

// First record start at or after `at`: an LF followed by a digit or a BOM. Inside quotes an
// LF only occurs in notes, which are lowercase words, so it is never followed by either.
fn boundary(path: &Path, at: u64, len: u64) -> u64 {
    let mut f = File::open(path).unwrap();
    f.seek(SeekFrom::Start(at - 1)).unwrap();
    let mut w = Vec::new();
    f.take(1 << 16).read_to_end(&mut w).unwrap();
    w.windows(2)
        .position(|x| x[0] == b'\n' && (x[1].is_ascii_digit() || x[1] == BOM[0]))
        .map_or(len, |i| at + i as u64)
}

fn money(c: i64) -> String {
    let sign = if c < 0 { "-" } else { "" };
    let a = c.unsigned_abs();
    format!("{sign}{}.{:02}", a / 100, a % 100)
}

fn main() {
    let dir = |var, default| std::env::var_os(var).map_or(default, |v| v.into_string().unwrap());
    let data = dir("SPEED_DEMON_DATA_DIR", "/data".into());
    let out_dir = dir("SPEED_DEMON_OUTPUT_DIR", "/out".into());
    let threads = thread::available_parallelism().map_or(12, |n| n.get()) as u64;

    // regions.csv: plain lines, region ids ascending. Countries are indexed in sorted order,
    // and _UNMATCHED takes the last index since '_' sorts after every capital letter.
    let rtext = std::fs::read(Path::new(&data).join("regions.csv")).unwrap();
    let mut lines = rtext.split(|&c| c == b'\n').filter(|l| !l.is_empty());
    assert_eq!(lines.next(), Some(&b"region_id,country"[..]), "unexpected regions.csv header");
    let rows: Vec<(usize, &[u8])> = lines
        .map(|l| {
            let c = l.iter().position(|&c| c == b',').unwrap();
            (std::str::from_utf8(&l[..c]).unwrap().parse().unwrap(), &l[c + 1..])
        })
        .collect();
    let mut keys: Vec<&[u8]> = rows.iter().map(|r| r.1).collect();
    keys.sort_unstable();
    keys.dedup();
    let idx: FxMap<&[u8], u16> = keys.iter().enumerate().map(|(i, &k)| (k, i as u16)).collect();
    let mut region = vec![u16::MAX; rows.iter().map(|r| r.0 + 1).max().unwrap_or(0)];
    for &(r, k) in &rows {
        region[r] = idx[k];
    }
    keys.push(b"_UNMATCHED");
    let groups = keys.len();

    // records.csv: skip the BOM and header, then split the rest at record starts.
    let path = Path::new(&data).join("records.csv");
    let len = std::fs::metadata(&path).unwrap().len();
    let mut head = Vec::new();
    File::open(&path).unwrap().take(256).read_to_end(&mut head).unwrap();
    let h = head.strip_prefix(BOM).unwrap_or(&head);
    let lf = h.iter().position(|&c| c == b'\n').expect("no header terminator");
    assert_eq!(h[..lf].strip_suffix(b"\r").unwrap_or(&h[..lf]), HEADER, "unexpected records.csv header");
    let first = (head.len() - h.len() + lf + 1) as u64;
    let mut cuts = vec![first];
    for t in 1..threads {
        let at = (len * t / threads).max(first + 1);
        let b = if at >= len { len } else { boundary(&path, at, len) };
        cuts.push(b.max(*cuts.last().unwrap()));
    }
    cuts.push(len);

    let parts: Vec<Part> = thread::scope(|s| {
        let hs: Vec<_> = cuts
            .windows(2)
            .map(|w| {
                let (path, region) = (&path, &region);
                s.spawn(move || scan(path, w[0], w[1], region, groups))
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });

    // Merge: map each thread's name ids to global ones, then OR the per-group bitsets.
    let mut aggs = vec![Agg::new(); groups];
    let mut global: FxMap<&[u8], u32> = FxMap::default();
    let mut seen: Vec<Vec<u64>> = vec![Vec::new(); groups];
    for part in &parts {
        let mut map = vec![0u32; part.names.len()];
        for (k, &v) in &part.names {
            let n = global.len() as u32;
            map[v as usize] = *global.entry(k.as_slice()).or_insert(n);
        }
        for g in 0..groups {
            let (a, b) = (&mut aggs[g], &part.aggs[g]);
            a.count += b.count;
            a.id_sum += b.id_sum;
            a.sum += b.sum;
            a.min = a.min.min(b.min);
            a.max = a.max.max(b.max);
            a.name_bytes += b.name_bytes;
            a.note_bytes += b.note_bytes;
            let s = &mut seen[g];
            for (w, &bits) in part.seen[g].iter().enumerate() {
                let mut bits = bits;
                while bits != 0 {
                    let n = map[w * 64 + bits.trailing_zeros() as usize] as usize;
                    bits &= bits - 1;
                    if s.len() <= n / 64 {
                        s.resize(n / 64 + 1, 0);
                    }
                    s[n / 64] |= 1 << (n % 64);
                }
            }
        }
    }

    let mut out = Vec::new();
    for (g, a) in aggs.iter().enumerate() {
        if a.count == 0 {
            continue;
        }
        let distinct: u32 = seen[g].iter().map(|w| w.count_ones()).sum();
        out.extend_from_slice(keys[g]);
        writeln!(
            out,
            " {} {} {} {} {} {} {} {}",
            a.count,
            a.id_sum,
            money(a.sum),
            money(a.min),
            money(a.max),
            distinct,
            a.name_bytes,
            a.note_bytes
        )
        .unwrap();
    }
    std::fs::create_dir_all(&out_dir).unwrap();
    std::fs::write(Path::new(&out_dir).join("aggregates.txt"), out).unwrap();
}
