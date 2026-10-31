use crate::index::InvertedIndex;
use crate::parser::{BoolOp, Query};

// Intersect two sorted, unique document-ID lists.
fn intersect(a: &[u32], b: &[u32]) -> Vec<u32> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }

    // For highly skewed lists, binary searches avoid scanning the much
    // larger list. The threshold is deliberately conservative.
    if a.len() <= b.len() / 8 {
        let mut result = Vec::with_capacity(a.len().min(b.len()));
        let mut lower = 0usize;

        for &doc_id in a {
            if lower >= b.len() {
                break;
            }

            match b[lower..].binary_search(&doc_id) {
                Ok(offset) => {
                    let found = lower + offset;
                    result.push(doc_id);
                    lower = found + 1;
                }
                Err(offset) => lower += offset,
            }
        }

        return result;
    }

    if b.len() <= a.len() / 8 {
        return intersect(b, a);
    }

    let mut result = Vec::with_capacity(a.len().min(b.len()));
    let (mut i, mut j) = (0usize, 0usize);

    while i < a.len() && j < b.len() {
        if a[i] < b[j] {
            i += 1;
        } else if a[i] > b[j] {
            j += 1;
        } else {
            result.push(a[i]);
            i += 1;
            j += 1;
        }
    }

    result
}

// Union of two sorted, unique document-ID lists.
fn union(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut result = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0usize, 0usize);

    while i < a.len() && j < b.len() {
        if a[i] < b[j] {
            result.push(a[i]);
            i += 1;
        } else if a[i] > b[j] {
            result.push(b[j]);
            j += 1;
        } else {
            result.push(a[i]);
            i += 1;
            j += 1;
        }
    }

    result.extend_from_slice(&a[i..]);
    result.extend_from_slice(&b[j..]);
    result
}

// A NOT B = A AND NOT B.
fn difference(a: &[u32], b: &[u32]) -> Vec<u32> {
    if a.is_empty() || b.is_empty() {
        return a.to_vec();
    }

    // When B is much larger, binary-search each value from A.
    if a.len() <= b.len() / 8 {
        let mut result = Vec::with_capacity(a.len());
        for &doc_id in a {
            if b.binary_search(&doc_id).is_err() {
                result.push(doc_id);
            }
        }
        return result;
    }

    let mut result = Vec::with_capacity(a.len());
    let mut j = 0usize;

    for &doc_id in a {
        while j < b.len() && b[j] < doc_id {
            j += 1;
        }

        if j == b.len() || b[j] != doc_id {
            result.push(doc_id);
        }
    }

    result
}

// Retrieve the sorted document IDs for a term.
fn get_posting<'a>(index: &'a InvertedIndex, term: &[u8]) -> &'a [u32] {
    match index.term_ids.get(term) {
        Some(&term_id) => &index.postings[term_id].docs,
        None => &[],
    }
}

// Boolean operators must be evaluated strictly left-to-right.
fn execute_bool(
    index: &InvertedIndex,
    terms: &[Vec<u8>],
    operators: &[BoolOp],
) -> Vec<u32> {
    if terms.is_empty() {
        return Vec::new();
    }

    let mut result = get_posting(index, &terms[0]).to_vec();

    for (i, operator) in operators.iter().enumerate() {
        let next = get_posting(index, &terms[i + 1]);

        result = match operator {
            BoolOp::And => intersect(&result, next),
            BoolOp::Or => union(&result, next),
            BoolOp::Not => difference(&result, next),
        };
    }

    result
}

// Exact consecutive-token phrase matching.
//
// Optimization: use the rarest phrase term as the candidate-document anchor.
// This can substantially reduce candidate documents for common first terms.
// Position offsets are calculated relative to the anchor term, so phrase
// order and exact adjacency remain unchanged.
fn execute_phrase(index: &InvertedIndex, terms: &[Vec<u8>]) -> Vec<u32> {
    if !(2..=8).contains(&terms.len()) {
        return Vec::new();
    }

    // Resolve term IDs once per query rather than once per candidate document.
    let mut term_ids = Vec::with_capacity(terms.len());

    for term in terms {
        match index.term_ids.get(term.as_slice()) {
            Some(&id) => term_ids.push(id),
            None => return Vec::new(),
        }
    }

    // All phrase terms should have positional postings, because the index
    // builder marks every term appearing in a phrase.
    let mut anchor_index = 0usize;
    let mut anchor_len = usize::MAX;

    for (i, &term_id) in term_ids.iter().enumerate() {
        let posting = &index.postings[term_id];

        if posting.positions.is_none() {
            return Vec::new();
        }

        if posting.docs.len() < anchor_len {
            anchor_len = posting.docs.len();
            anchor_index = i;
        }
    }

    let anchor_posting = &index.postings[term_ids[anchor_index]];
    let anchor_positions = match &anchor_posting.positions {
        Some(positions) => positions,
        None => return Vec::new(),
    };

    let mut result = Vec::new();

    // A phrase has at most eight terms. A fixed-size stack array avoids
    // allocating a Vec of position-list references for every candidate doc.
    'document: for (anchor_doc_index, &doc_id) in
        anchor_posting.docs.iter().enumerate()
    {
        let mut position_lists: [&[u32]; 8] = [&[]; 8];

        for term_index in 0..terms.len() {
            if term_index == anchor_index {
                position_lists[term_index] =
                    &anchor_positions[anchor_doc_index];
                continue;
            }

            let posting = &index.postings[term_ids[term_index]];

            let positions = match &posting.positions {
                Some(positions) => positions,
                None => continue 'document,
            };

            let doc_index = match posting.docs.binary_search(&doc_id) {
                Ok(i) => i,
                Err(_) => continue 'document,
            };

            position_lists[term_index] = &positions[doc_index];
        }

        // Each anchor occurrence implies exactly one possible start position.
        'occurrence: for &anchor_position in position_lists[anchor_index] {
            for term_index in 0..terms.len() {
                if term_index == anchor_index {
                    continue;
                }

                let expected = anchor_position as i64
                    + term_index as i64
                    - anchor_index as i64;

                if expected < 0 || expected > u32::MAX as i64 {
                    continue 'occurrence;
                }

                if position_lists[term_index]
                    .binary_search(&(expected as u32))
                    .is_err()
                {
                    continue 'occurrence;
                }
            }

            result.push(doc_id);
            continue 'document;
        }
    }

    result
}

// Execute one parsed query.
pub fn execute_query(index: &InvertedIndex, query: &Query) -> Vec<u32> {
    match query {
        Query::Bool { terms, operators } => {
            execute_bool(index, terms, operators)
        }
        Query::Phrase { terms } => execute_phrase(index, terms),
    }
}

// Format: count sum first_id ... tenth_id.
pub fn format_result(doc_ids: &[u32]) -> String {
    let count = doc_ids.len();
    let sum: u64 = doc_ids.iter().map(|&id| id as u64).sum();

    let mut output = format!("{} {}", count, sum);

    for &doc_id in doc_ids.iter().take(10) {
        output.push(' ');
        output.push_str(&doc_id.to_string());
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intersection_works() {
        assert_eq!(intersect(&[1, 2, 4], &[2, 3, 4]), vec![2, 4]);
    }

    #[test]
    fn intersection_handles_skewed_lists() {
        let small = vec![2, 100, 900];
        let large: Vec<u32> = (0..1000).collect();
        assert_eq!(intersect(&small, &large), small);
        assert_eq!(intersect(&large, &[2, 100, 900]), small);
    }

    #[test]
    fn union_works() {
        assert_eq!(union(&[1, 2, 4], &[2, 3, 4]), vec![1, 2, 3, 4]);
    }

    #[test]
    fn difference_works() {
        assert_eq!(difference(&[1, 2, 3, 4], &[2, 4]), vec![1, 3]);
    }

    #[test]
    fn difference_with_empty_list() {
        assert_eq!(difference(&[1, 2, 3], &[]), vec![1, 2, 3]);
    }

    #[test]
    fn difference_with_empty_left_list() {
        assert_eq!(difference(&[], &[1, 2, 3]), Vec::<u32>::new());
    }

    #[test]
    fn format_result_works() {
        assert_eq!(format_result(&[1, 3, 5]), "3 9 1 3 5");
    }

    #[test]
    fn format_result_limits_ids_to_ten() {
        let ids: Vec<u32> = (0..15).collect();
        assert_eq!(format_result(&ids), "15 105 0 1 2 3 4 5 6 7 8 9");
    }
}
