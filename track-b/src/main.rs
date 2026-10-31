use std::collections::HashMap;
use std::fs::File;
use std::hash::{BuildHasherDefault, Hasher};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

// Fx-style hasher: terms are short ASCII, SipHash is overkill.
#[derive(Default)]
struct Fx(u64);

impl Hasher for Fx {
    fn write(&mut self, bytes: &[u8]) {
        for c in bytes.chunks(8) {
            let mut w = [0u8; 8];
            w[..c.len()].copy_from_slice(c);
            self.0 = (self.0.rotate_left(5) ^ u64::from_le_bytes(w)).wrapping_mul(0x517c_c1b7_2722_0a95);
        }
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

/// Term -> (posting id, phrase id). Phrase ids are dense over phrase terms only
/// so token streams fit in u16: the spec caps phrase term uses at 19,000 x 8.
type Terms<'a> = HashMap<&'a [u8], (u32, u16), BuildHasherDefault<Fx>>;
const NO_PID: u16 = u16::MAX;

enum Query {
    Bool(Vec<u32>, Vec<u8>),
    Phrase(Vec<u32>, Vec<u16>),
}

/// Documents `first..`; doc `first + i` has token stream `toks[starts[i]..starts[i + 1]]`
/// (phrase ids, `NO_PID` for other tokens) and `post[term]` is a sorted doc id list.
#[derive(Default)]
struct Slice {
    first: u32,
    toks: Vec<u16>,
    starts: Vec<u32>,
    post: Vec<Vec<u32>>,
}

fn intern<'a>(terms: &mut Terms<'a>, pids: &mut u16, w: &'a [u8], phrase: bool) -> (u32, u16) {
    let n = terms.len() as u32;
    let t = terms.entry(w).or_insert((n, NO_PID));
    if phrase && t.1 == NO_PID {
        (t.1, *pids) = (*pids, *pids + 1);
    }
    *t
}

fn parse(input: &[u8]) -> (Vec<Query>, Terms<'_>) {
    let (mut terms, mut pids, mut queries) = (Terms::default(), 0, Vec::new());
    for line in input.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
        queries.push(if let Some(rest) = line.strip_prefix(b"BOOL ") {
            let words: Vec<&[u8]> = rest.split(|&b| b == b' ').collect();
            let ts = words.iter().step_by(2).map(|w| intern(&mut terms, &mut pids, w, false).0);
            Query::Bool(ts.collect(), words.iter().skip(1).step_by(2).map(|w| w[0]).collect())
        } else {
            let rest = &line[8..line.len() - 1]; // PHRASE "..."
            let ts: Vec<_> = rest.split(|&b| b == b' ').map(|w| intern(&mut terms, &mut pids, w, true)).collect();
            Query::Phrase(ts.iter().map(|t| t.0).collect(), ts.iter().map(|t| t.1).collect())
        });
    }
    (queries, terms)
}

/// Index the lines of `path` that start in `start..end`, streaming the file.
fn index_range(path: &Path, start: u64, end: u64, terms: &Terms) -> Slice {
    let mut sl = Slice { toks: Vec::with_capacity((end - start) as usize / 6), ..Default::default() };
    sl.post = vec![Vec::new(); terms.len()];
    let mut f = File::open(path).unwrap();
    // Begin one byte early and drop the first (partial) line, so a line that
    // starts exactly at `start` is kept.
    let mut off = start.saturating_sub(1);
    f.seek(SeekFrom::Start(off)).unwrap();
    let mut skip = start > 0;
    let (mut buf, mut have) = (vec![0u8; 1 << 20], 0);
    'read: loop {
        if have == buf.len() {
            buf.resize(have * 2, 0);
        }
        let n = f.read(&mut buf[have..]).unwrap();
        have += n;
        let mut i = 0;
        while i < have {
            let stop = match buf[i..have].iter().position(|&b| b == b'\n') {
                Some(p) => i + p,
                None if n == 0 => have,
                None => break,
            };
            if off + i as u64 >= end && !skip {
                break 'read;
            }
            if !skip {
                index_line(&mut sl, &buf[i..stop], terms);
            }
            (skip, i) = (false, stop + 1);
        }
        if n == 0 {
            break;
        }
        buf.copy_within(i..have, 0);
        (off, have) = (off + i as u64, have - i);
    }
    sl.starts.push(sl.toks.len() as u32);
    sl
}

fn index_line(sl: &mut Slice, line: &[u8], terms: &Terms) {
    let Some(tab) = line.iter().position(|&b| b == b'\t') else { return };
    let doc = line[..tab].iter().fold(0u32, |a, &b| a * 10 + (b - b'0') as u32);
    if sl.starts.is_empty() {
        sl.first = doc;
    }
    sl.starts.push(sl.toks.len() as u32);
    let (mut buf, mut len) = ([0u8; 33], 0);
    for &b in line[tab + 1..].iter().chain([&b' ']) {
        if b.is_ascii_alphanumeric() {
            buf[len.min(32)] = b.to_ascii_lowercase();
            len += 1;
        } else if len > 0 {
            // Tokens over 32 bytes never equal a query term.
            let t = if len <= 32 { terms.get(&buf[..len]) } else { None };
            sl.toks.push(t.map_or(NO_PID, |t| t.1));
            if let Some(&(id, _)) = t {
                let p = &mut sl.post[id as usize];
                if p.last() != Some(&doc) {
                    p.push(doc);
                }
            }
            len = 0;
        }
    }
}

/// Elements of `a` that are (`want`) or are not in `b`; binary search when `b` is much larger.
fn filter(a: &[u32], b: &[u32], want: bool) -> Vec<u32> {
    let (gallop, mut rest) = (a.len() * 8 <= b.len(), b);
    let mut keep = |&x: &u32| {
        let i = if gallop { rest.partition_point(|&y| y < x) } else { rest.iter().take_while(|&&y| y < x).count() };
        rest = &rest[i..];
        (rest.first() == Some(&x)) == want
    };
    a.iter().copied().filter(|x| keep(x)).collect()
}

fn union(a: &[u32], b: &[u32]) -> Vec<u32> {
    let (mut out, mut i, mut j) = (Vec::with_capacity(a.len() + b.len()), 0, 0);
    while i < a.len() && j < b.len() {
        out.push(a[i].min(b[j]));
        (i, j) = (i + (a[i] <= b[j]) as usize, j + (b[j] <= a[i]) as usize);
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
    out
}

fn and(a: &[u32], b: &[u32]) -> Vec<u32> {
    if a.len() <= b.len() { filter(a, b, true) } else { filter(b, a, true) }
}

fn run(s: &Slice, q: &Query) -> Vec<u32> {
    match q {
        Query::Bool(terms, ops) => {
            let mut r = s.post[terms[0] as usize].clone();
            for (op, &t) in ops.iter().zip(&terms[1..]) {
                let p = &s.post[t as usize];
                r = match op {
                    b'A' => and(&r, p),
                    b'O' => union(&r, p),
                    _ => filter(&r, p, false),
                };
            }
            r
        }
        // Docs holding every term, then match the token window around the rarest term.
        Query::Phrase(terms, pids) => {
            let r = (0..terms.len()).min_by_key(|&k| s.post[terms[k] as usize].len()).unwrap();
            let mut cand = s.post[terms[r] as usize].clone();
            for &t in terms {
                cand = and(&cand, &s.post[t as usize]);
            }
            cand.retain(|&d| {
                let i = (d - s.first) as usize;
                let seq = &s.toks[s.starts[i] as usize..s.starts[i + 1] as usize];
                (r..(seq.len() + r + 1).saturating_sub(pids.len()))
                    .any(|j| seq[j] == pids[r] && seq[j - r..j - r + pids.len()] == pids[..])
            });
            cand
        }
    }
}

fn main() {
    let dir = |var, default| std::env::var_os(var).map_or(default, |v| v.into_string().unwrap());
    let data = dir("SPEED_DEMON_DATA_DIR", "/data".into());
    let out_dir = dir("SPEED_DEMON_OUTPUT_DIR", "/out".into());
    let threads = thread::available_parallelism().map_or(12, |n| n.get());
    let qtext = std::fs::read(Path::new(&data).join("queries.txt")).unwrap();
    let (qs, terms) = parse(&qtext);

    // One slice per thread over its own byte range. Slices hold disjoint
    // ascending doc ranges, so they are never merged: queries run per slice.
    let path = Path::new(&data).join("docs.txt");
    let len = std::fs::metadata(&path).unwrap().len();
    let t = threads as u64;
    let ix: Vec<Slice> = thread::scope(|s| {
        let hs: Vec<_> = (0..t)
            .map(|i| {
                let (path, terms) = (&path, &terms);
                s.spawn(move || index_range(path, len * i / t, len * (i + 1) / t, terms))
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
                        let mut buf = Vec::new();
                        for q in &qs[b * 64..(b * 64 + 64).min(qs.len())] {
                            let (mut count, mut sum, mut first) = (0, 0u64, Vec::<u32>::new());
                            for s in &ix {
                                let ids = run(s, q);
                                count += ids.len();
                                sum += ids.iter().map(|&x| x as u64).sum::<u64>();
                                first.extend(ids.iter().take(10 - first.len()));
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
