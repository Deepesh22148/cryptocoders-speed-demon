use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

// Fx-style hasher: terms are short ASCII, SipHash is overkill.
#[derive(Default)]
pub struct Fx(u64);

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

pub type TermMap<'a> = HashMap<&'a [u8], u32, BuildHasherDefault<Fx>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    And,
    Or,
    Not,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Query {
    Bool { terms: Vec<u32>, ops: Vec<Op> },
    Phrase(Vec<u32>),
}

/// Parsed queries with every term interned to an id.
pub struct Parsed<'a> {
    pub queries: Vec<Query>,
    pub terms: TermMap<'a>,
}

pub fn parse(input: &[u8]) -> Parsed<'_> {
    let mut p = Parsed { queries: Vec::new(), terms: TermMap::default() };
    for line in input.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
        let q = if let Some(rest) = line.strip_prefix(b"BOOL ") {
            let mut terms = Vec::new();
            let mut ops = Vec::new();
            for (i, w) in rest.split(|&b| b == b' ').enumerate() {
                if i % 2 == 0 {
                    terms.push(intern(&mut p, w));
                } else {
                    ops.push(match w {
                        b"AND" => Op::And,
                        b"OR" => Op::Or,
                        b"NOT" => Op::Not,
                        _ => panic!("bad operator in {:?}", String::from_utf8_lossy(line)),
                    });
                }
            }
            Query::Bool { terms, ops }
        } else if let Some(rest) = line.strip_prefix(b"PHRASE \"").and_then(|r| r.strip_suffix(b"\"")) {
            Query::Phrase(rest.split(|&b| b == b' ').map(|w| intern(&mut p, w)).collect())
        } else {
            panic!("bad query {:?}", String::from_utf8_lossy(line));
        };
        p.queries.push(q);
    }
    p
}

fn intern<'a>(p: &mut Parsed<'a>, term: &'a [u8]) -> u32 {
    let next = p.terms.len() as u32;
    *p.terms.entry(term).or_insert(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_interns() {
        let p = parse(b"BOOL a OR b AND a\nPHRASE \"b c\"\n");
        assert_eq!(p.queries[0], Query::Bool { terms: vec![0, 1, 0], ops: vec![Op::Or, Op::And] });
        assert_eq!(p.queries[1], Query::Phrase(vec![1, 2]));
    }
}
