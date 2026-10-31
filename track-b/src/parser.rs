
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoolOp {
    And,
    Or,
    Not,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Query {
    Bool {
        terms: Vec<Vec<u8>>,
        operators: Vec<BoolOp>,
    },
    Phrase {
        terms: Vec<Vec<u8>>,
    },
}

fn is_valid_term(term: &[u8]) -> bool {
    !term.is_empty()
        && term.len() <= 32
        && term.iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

fn parse_operator(op: &[u8]) -> Option<BoolOp> {
    match op {
        b"AND" => Some(BoolOp::And),
        b"OR" => Some(BoolOp::Or),
        b"NOT" => Some(BoolOp::Not),
        _ => None,
    }
}

pub fn parse_query(line: &[u8]) -> Result<Query, &'static str> {
    if line.starts_with(b"BOOL ") {
        return parse_bool(&line[5..]);
    }

    if line.starts_with(b"PHRASE ") {
        return parse_phrase(&line[7..]);
    }

    Err("Unknown query type")
}

fn parse_bool(input: &[u8]) -> Result<Query, &'static str> {
    let parts: Vec<&[u8]> = input.split(|&b| b == b' ').collect();

    if parts.is_empty() || parts.len() % 2 == 0 {
        return Err("Invalid boolean query structure");
    }

    let term_count = (parts.len() + 1) / 2;

    if !(1..=6).contains(&term_count) {
        return Err("Boolean query must contain 1 to 6 terms");
    }

    let mut terms = Vec::with_capacity(term_count);
    let mut operators = Vec::with_capacity(term_count - 1);

    for i in 0..parts.len() {
        if i % 2 == 0 {
            if !is_valid_term(parts[i]) {
                return Err("Invalid boolean term");
            }

            terms.push(parts[i].to_vec());
        } else {
            let op = parse_operator(parts[i])
                .ok_or("Invalid boolean operator")?;

            operators.push(op);
        }
    }

    Ok(Query::Bool { terms, operators })
}

fn parse_phrase(input: &[u8]) -> Result<Query, &'static str> {
    if input.len() < 5 || !input.starts_with(b"\"") || !input.ends_with(b"\"") {
        return Err("Invalid phrase format");
    }

    let content = &input[1..input.len() - 1];

    let parts: Vec<&[u8]> = content.split(|&b| b == b' ').collect();

    if !(2..=8).contains(&parts.len()) {
        return Err("Phrase must contain 2 to 8 terms");
    }

    let mut terms = Vec::with_capacity(parts.len());

    for term in parts {
        if !is_valid_term(term) {
            return Err("Invalid phrase term");
        }

        terms.push(term.to_vec());
    }

    Ok(Query::Phrase { terms })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_term_bool() {
        let query = parse_query(b"BOOL python").unwrap();

        assert_eq!(
            query,
            Query::Bool {
                terms: vec![b"python".to_vec()],
                operators: vec![],
            }
        );
    }

    #[test]
    fn parse_multiple_bool_terms() {
        let query = parse_query(b"BOOL python AND machine OR learning").unwrap();

        assert_eq!(
            query,
            Query::Bool {
                terms: vec![
                    b"python".to_vec(),
                    b"machine".to_vec(),
                    b"learning".to_vec(),
                ],
                operators: vec![BoolOp::And, BoolOp::Or],
            }
        );
    }

    #[test]
    fn preserve_left_to_right_operator_order() {
        let query = parse_query(b"BOOL a OR b AND c").unwrap();

        match query {
            Query::Bool { operators, .. } => {
                assert_eq!(operators, vec![BoolOp::Or, BoolOp::And]);
            }
            _ => panic!("Expected boolean query"),
        }
    }

    #[test]
    fn parse_phrase() {
        let query = parse_query(b"PHRASE \"writ of habeas corpus\"").unwrap();

        assert_eq!(
            query,
            Query::Phrase {
                terms: vec![
                    b"writ".to_vec(),
                    b"of".to_vec(),
                    b"habeas".to_vec(),
                    b"corpus".to_vec(),
                ]
            }
        );
    }

    #[test]
    fn reject_invalid_queries() {
        assert!(parse_query(b"BOOL python XOR machine").is_err());
        assert!(parse_query(b"BOOL python AND").is_err());
        assert!(parse_query(b"BOOL Python").is_err());
        assert!(parse_query(b"PHRASE \"python\"").is_err());
        assert!(parse_query(b"PHRASE \"python  machine\"").is_err());
    }

    #[test]
    fn reject_empty_boolean_terms() {
        assert!(parse_query(b"BOOL python  AND machine").is_err());
    }
}
