use std::thread;

use crate::parser::TermMap;

const MAX_TERM: usize = 32;

/// Index over a contiguous range of documents starting at `first`.
pub struct Slice {
    pub first: u32,
    /// Every token of the slice as a query-term id (`NONE` if not a query
    /// term); doc `first + i` is `toks[starts[i]..starts[i + 1]]`.
    pub toks: Vec<u32>,
    pub starts: Vec<u32>,
    /// Sorted doc ids per query term.
    pub post: Vec<Vec<u32>>,
}

pub const NONE: u32 = u32::MAX;

// Byte -> lowercased token byte, or 0 for a separator.
const LUT: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut b = 0;
    while b < 256 {
        let c = (b as u8).to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            t[b] = c;
        }
        b += 1;
    }
    t
};

/// One slice per thread. Slices hold disjoint ascending doc ranges, so they
/// are never merged: queries run per slice and concatenate.
pub fn build(docs: &[u8], terms: &TermMap, threads: usize) -> Vec<Slice> {
    let mut cuts = vec![0];
    for t in 1..threads {
        let mut p = (docs.len() * t / threads).max(*cuts.last().unwrap());
        while p < docs.len() && docs[p - 1] != b'\n' {
            p += 1;
        }
        cuts.push(p);
    }
    cuts.push(docs.len());
    thread::scope(|s| {
        let hs: Vec<_> = cuts.windows(2).map(|w| s.spawn(move || index_slice(&docs[w[0]..w[1]], terms))).collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    })
}

fn index_slice(text: &[u8], terms: &TermMap) -> Slice {
    let mut post: Vec<Vec<u32>> = vec![Vec::new(); terms.len()];
    let mut toks = Vec::with_capacity(text.len() / 6);
    let mut starts = vec![];
    let mut first = None;
    let mut add = |toks: &mut Vec<u32>, tok: &[u8], doc: u32| {
        let id = terms.get(tok).map_or(NONE, |&id| id);
        toks.push(id);
        if id != NONE {
            let p = &mut post[id as usize];
            if p.last() != Some(&doc) {
                p.push(doc);
            }
        }
    };
    for line in text.split(|&b| b == b'\n') {
        let Some(tab) = line.iter().position(|&b| b == b'\t') else { continue };
        let doc = line[..tab].iter().fold(0u32, |a, &b| a * 10 + (b - b'0') as u32);
        first.get_or_insert(doc);
        let body = &line[tab + 1..];
        starts.push(toks.len() as u32);
        let mut buf = [0u8; MAX_TERM];
        let mut len = 0;
        for &b in body {
            let c = LUT[b as usize];
            if c != 0 {
                if len < MAX_TERM {
                    buf[len] = c;
                }
                len += 1;
            } else if len > 0 {
                // Longer tokens can never equal a query term.
                add(&mut toks, if len <= MAX_TERM { &buf[..len] } else { b"" }, doc);
                len = 0;
            }
        }
        if len > 0 {
            add(&mut toks, if len <= MAX_TERM { &buf[..len] } else { b"" }, doc);
        }
    }
    starts.push(toks.len() as u32);
    Slice { first: first.unwrap_or(0), toks, starts, post }
}
