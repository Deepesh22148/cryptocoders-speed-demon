use std::borrow::Cow;
use std::io::Write;

use crate::index::Slice;
use crate::parser::{Op, Query};

// Intersect two sorted, unique id lists; binary search when sizes are skewed.
fn intersect(a: &[u32], b: &[u32]) -> Vec<u32> {
    let (a, b) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let mut out = Vec::with_capacity(a.len());
    if a.len() * 8 <= b.len() {
        let mut rest = b;
        for &x in a {
            let i = rest.partition_point(|&y| y < x);
            if i < rest.len() && rest[i] == x {
                out.push(x);
            }
            rest = &rest[i..];
        }
        return out;
    }
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        let (x, y) = (a[i], b[j]);
        if x == y {
            out.push(x);
        }
        i += (x <= y) as usize;
        j += (y <= x) as usize;
    }
    out
}

fn union(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        let (x, y) = (a[i], b[j]);
        out.push(x.min(y));
        i += (x <= y) as usize;
        j += (y <= x) as usize;
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
    out
}

// a \ b
fn difference(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(a.len());
    if a.len() * 8 <= b.len() {
        let mut rest = b;
        for &x in a {
            let i = rest.partition_point(|&y| y < x);
            if i == rest.len() || rest[i] != x {
                out.push(x);
            }
            rest = &rest[i..];
        }
        return out;
    }
    let mut j = 0;
    for &x in a {
        while j < b.len() && b[j] < x {
            j += 1;
        }
        if j == b.len() || b[j] != x {
            out.push(x);
        }
    }
    out
}

fn run_bool<'a>(post: &'a [Vec<u32>], terms: &[u32], ops: &[Op]) -> Cow<'a, [u32]> {
    let mut r = Cow::Borrowed(&post[terms[0] as usize][..]);
    for (op, &t) in ops.iter().zip(&terms[1..]) {
        let s = &post[t as usize];
        r = Cow::Owned(match op {
            Op::And => intersect(&r, s),
            Op::Or => union(&r, s),
            Op::Not => difference(&r, s),
        });
    }
    r
}

// Docs holding every term, rarest list first, then an exact check of each
// one's token-id stream. Phrase answers are tiny, so no positions are kept.
fn run_phrase(s: &Slice, terms: &[u32]) -> Vec<u32> {
    let mut lists: Vec<&[u32]> = terms.iter().map(|&t| &s.post[t as usize][..]).collect();
    lists.sort_unstable_by_key(|l| l.len());
    let mut cand = lists[0].to_vec();
    for l in &lists[1..] {
        if cand.is_empty() {
            break;
        }
        cand = intersect(&cand, l);
    }
    // Find the rarest term in the stream, then compare the window around it.
    let r = (0..terms.len()).min_by_key(|&k| s.post[terms[k] as usize].len()).unwrap();
    cand.retain(|&d| {
        let i = (d - s.first) as usize;
        let seq = &s.toks[s.starts[i] as usize..s.starts[i + 1] as usize];
        (r..(seq.len() + r + 1).saturating_sub(terms.len()))
            .any(|j| seq[j] == terms[r] && &seq[j - r..j - r + terms.len()] == terms)
    });
    cand
}

/// Append `count sum id1..id10\n` for one query, answered slice by slice.
pub fn answer(slices: &[Slice], q: &Query, out: &mut Vec<u8>) {
    let (mut count, mut sum) = (0usize, 0u64);
    let mut first: Vec<u32> = Vec::with_capacity(10);
    for s in slices {
        let ids: Cow<[u32]> = match q {
            Query::Bool { terms, ops } => run_bool(&s.post, terms, ops),
            Query::Phrase(terms) => Cow::Owned(run_phrase(s, terms)),
        };
        count += ids.len();
        sum += ids.iter().map(|&x| x as u64).sum::<u64>();
        let room = 10 - first.len();
        first.extend(ids.iter().take(room));
    }
    write!(out, "{count} {sum}").unwrap();
    for x in first {
        write!(out, " {x}").unwrap();
    }
    out.push(b'\n');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_ops() {
        let big: Vec<u32> = (0..1000).collect();
        assert_eq!(intersect(&[1, 2, 4], &[2, 3, 4]), vec![2, 4]);
        assert_eq!(intersect(&big, &[2, 100, 900]), vec![2, 100, 900]);
        assert_eq!(union(&[1, 2, 4], &[2, 3, 4]), vec![1, 2, 3, 4]);
        assert_eq!(difference(&[1, 2, 3, 4], &[2, 4]), vec![1, 3]);
        assert_eq!(difference(&[5, 2000], &big), vec![2000]);
        assert_eq!(difference(&[], &[1]), Vec::<u32>::new());
    }
}
