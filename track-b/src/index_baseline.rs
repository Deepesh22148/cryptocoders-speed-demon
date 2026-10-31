
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};

use crate::parser::Query;

#[derive(Debug, Default)]
pub struct TermPosting {
    pub docs: Vec<u32>,
    // One position list per document, only for phrase-query terms.
    pub positions: Option<Vec<Vec<u32>>>,
}

#[derive(Debug)]
pub struct InvertedIndex {
    pub terms: Vec<Vec<u8>>,
    pub postings: Vec<TermPosting>,
    pub term_ids: HashMap<Vec<u8>, usize>,
    pub document_count: u32,
}

pub fn collect_query_terms(queries: &[Query]) -> (HashSet<Vec<u8>>, HashSet<Vec<u8>>) {
    let mut all_terms = HashSet::new();
    let mut phrase_terms = HashSet::new();

    for query in queries {
        match query {
            Query::Bool { terms, .. } => {
                for term in terms {
                    all_terms.insert(term.clone());
                }
            }
            Query::Phrase { terms } => {
                for term in terms {
                    all_terms.insert(term.clone());
                    phrase_terms.insert(term.clone());
                }
            }
        }
    }

    (all_terms, phrase_terms)
}

pub fn build_index(
    docs_path: &str,
    queries: &[Query],
) -> Result<InvertedIndex, String> {
    let (all_terms, phrase_terms) = collect_query_terms(queries);

    let terms: Vec<Vec<u8>> = all_terms.into_iter().collect();

    let term_ids: HashMap<Vec<u8>, usize> = terms
        .iter()
        .enumerate()
        .map(|(id, term)| (term.clone(), id))
        .collect();

    let mut postings: Vec<TermPosting> = terms
        .iter()
        .map(|term| TermPosting {
            docs: Vec::new(),
            positions: if phrase_terms.contains(term) {
                Some(Vec::new())
            } else {
                None
            },
        })
        .collect();

    let file = File::open(docs_path)
        .map_err(|e| format!("Cannot open documents: {e}"))?;

    let mut reader = BufReader::new(file);
    let mut line = Vec::with_capacity(4096);
    let mut expected_id = 0u32;

    loop {
        line.clear();

        let bytes_read = reader
            .read_until(b'\n', &mut line)
            .map_err(|e| format!("Document read error: {e}"))?;

        if bytes_read == 0 {
            break;
        }

        if line.last() == Some(&b'\n') {
            line.pop();
        }

        let tab = line
            .iter()
            .position(|&b| b == b'\t')
            .ok_or("Missing document TAB separator")?;

        let doc_id: u32 = std::str::from_utf8(&line[..tab])
            .map_err(|_| "Invalid document ID")?
            .parse()
            .map_err(|_| "Invalid document ID")?;

        if doc_id != expected_id {
            return Err(format!(
                "Expected document {expected_id}, got {doc_id}"
            ));
        }

        let text = &line[tab + 1..];

        let mut position = 0u32;
        let mut i = 0usize;

        while i < text.len() {
            let b = text[i].to_ascii_lowercase();

            if b.is_ascii_lowercase() || b.is_ascii_digit() {
                let start = i;
                i += 1;

                while i < text.len() {
                    let next = text[i].to_ascii_lowercase();

                    if next.is_ascii_lowercase() || next.is_ascii_digit() {
                        i += 1;
                    } else {
                        break;
                    }
                }

                if let Some(&term_id) = term_ids.get(&text[start..i].iter()
                    .map(u8::to_ascii_lowercase)
                    .collect::<Vec<u8>>()[..])
                {
                    let posting = &mut postings[term_id];

                    if posting.docs.last() != Some(&doc_id) {
                        posting.docs.push(doc_id);

                        if let Some(positions) = &mut posting.positions {
                            positions.push(Vec::new());
                        }
                    }

                    if let Some(positions) = &mut posting.positions {
                        positions.last_mut().unwrap().push(position);
                    }
                }

                position += 1;
            } else {
                i += 1;
            }
        }

        expected_id += 1;
    }

    Ok(InvertedIndex {
        terms,
        postings,
        term_ids,
        document_count: expected_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_terms_from_queries() {
        let queries = vec![
            Query::Bool {
                terms: vec![b"python".to_vec(), b"machine".to_vec()],
                operators: vec![crate::parser::BoolOp::And],
            },
            Query::Phrase {
                terms: vec![b"machine".to_vec(), b"learning".to_vec()],
            },
        ];

        let (all, phrase) = collect_query_terms(&queries);

        assert_eq!(all.len(), 3);
        assert_eq!(phrase.len(), 2);
        assert!(phrase.contains(b"machine".as_slice()));
        assert!(phrase.contains(b"learning".as_slice()));
    }
}
