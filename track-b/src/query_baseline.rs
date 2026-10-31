
use crate::index::InvertedIndex;
use crate::parser::{BoolOp, Query};

// A AND B
fn intersect(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut result = Vec::new();
    let (mut i, mut j) = (0, 0);

    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                result.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }

    result
}

// A OR B
fn union(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut result = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);

    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => {
                result.push(a[i]);
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                result.push(b[j]);
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                result.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }

    result.extend_from_slice(&a[i..]);
    result.extend_from_slice(&b[j..]);

    result
}

// A NOT B = A AND NOT B
fn difference(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut result = Vec::new();
    let mut j = 0;

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
fn get_posting<'a>(
    index: &'a InvertedIndex,
    term: &[u8],
) -> &'a [u32] {
    match index.term_ids.get(term) {
        Some(&term_id) => &index.postings[term_id].docs,
        None => &[],
    }
}

// Execute Boolean operations strictly from left to right.
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
fn execute_phrase(
    index: &InvertedIndex,
    terms: &[Vec<u8>],
) -> Vec<u32> {
    if terms.len() < 2 {
        return Vec::new();
    }

    // Locate the first term.
    let first_id = match index.term_ids.get(terms[0].as_slice()) {
        Some(&id) => id,
        None => return Vec::new(),
    };

    let first_posting = &index.postings[first_id];

    let first_positions = match &first_posting.positions {
        Some(positions) => positions,
        None => return Vec::new(),
    };

    let mut result = Vec::new();

    // The first term determines the candidate documents.
    'document: for (doc_index, &doc_id) in
        first_posting.docs.iter().enumerate()
    {
        let mut position_lists: Vec<&[u32]> =
            Vec::with_capacity(terms.len());

        position_lists.push(&first_positions[doc_index]);

        // Find the positions of every remaining term in this document.
        for term in terms.iter().skip(1) {
            let term_id = match index.term_ids.get(term.as_slice()) {
                Some(&id) => id,
                None => continue 'document,
            };

            let posting = &index.postings[term_id];

            let positions = match &posting.positions {
                Some(p) => p,
                None => continue 'document,
            };

            let posting_doc_index =
                match posting.docs.binary_search(&doc_id) {
                    Ok(i) => i,
                    Err(_) => continue 'document,
                };

            position_lists.push(&positions[posting_doc_index]);
        }

        // Try every occurrence of the first term.
        'candidate: for &start in position_lists[0] {
            for (offset, positions) in
                position_lists.iter().enumerate().skip(1)
            {
                let expected = start + offset as u32;

                if positions.binary_search(&expected).is_err() {
                    // This start position failed.
                    // Try the next occurrence of the first term.
                    continue 'candidate;
                }
            }

            // Every term was found at the exact expected position.
            result.push(doc_id);
            continue 'document;
        }
    }

    result
}

// Execute one parsed query.
pub fn execute_query(
    index: &InvertedIndex,
    query: &Query,
) -> Vec<u32> {
    match query {
        Query::Bool { terms, operators } => {
            execute_bool(index, terms, operators)
        }

        Query::Phrase { terms } => {
            execute_phrase(index, terms)
        }
    }
}

// Format: count sum first_id ... tenth_id
pub fn format_result(doc_ids: &[u32]) -> String {
    let count = doc_ids.len();

    let sum: u64 = doc_ids
        .iter()
        .map(|&id| id as u64)
        .sum();

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
        assert_eq!(
            intersect(&[1, 2, 4], &[2, 3, 4]),
            vec![2, 4]
        );
    }

    #[test]
    fn union_works() {
        assert_eq!(
            union(&[1, 2, 4], &[2, 3, 4]),
            vec![1, 2, 3, 4]
        );
    }

    #[test]
    fn difference_works() {
        assert_eq!(
            difference(&[1, 2, 3, 4], &[2, 4]),
            vec![1, 3]
        );
    }

    #[test]
    fn difference_with_empty_list() {
        assert_eq!(
            difference(&[1, 2, 3], &[]),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn difference_with_empty_left_list() {
        assert_eq!(
            difference(&[], &[1, 2, 3]),
            Vec::<u32>::new()
        );
    }

    #[test]
    fn format_result_works() {
        assert_eq!(
            format_result(&[1, 3, 5]),
            "3 9 1 3 5"
        );
    }

    #[test]
    fn format_result_limits_ids_to_ten() {
        let ids: Vec<u32> = (0..15).collect();

        assert_eq!(
            format_result(&ids),
            "15 105 0 1 2 3 4 5 6 7 8 9"
        );
    }
}
