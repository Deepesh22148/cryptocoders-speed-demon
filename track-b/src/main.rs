
mod parser;
mod index;
mod query;

use std::path::PathBuf;
use std::time::Instant;

fn tokenize(text: &[u8]) -> Vec<Vec<u8>> {
    let mut tokens = Vec::new();
    let mut current = Vec::new();

    for &byte in text {
        let lower = byte.to_ascii_lowercase();

        if lower.is_ascii_lowercase() || lower.is_ascii_digit() {
            current.push(lower);
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

fn env_path_or_default(variable: &str, default: &str) -> PathBuf {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}

fn main() {
    // Competition defaults: /data and /out.
    // Local testing can override these using environment variables.
    let data_dir = env_path_or_default("SPEED_DEMON_DATA_DIR", "/data");
    let output_dir = env_path_or_default("SPEED_DEMON_OUTPUT_DIR", "/out");

    let queries_path = data_dir.join("queries.txt");
    let docs_path = data_dir.join("docs.txt");
    let output_path = output_dir.join("results.txt");

    let total_start = Instant::now();

    // Stage 1: Read and parse queries.
    let query_start = Instant::now();

    println!("Reading queries from {}...", queries_path.display());

    let query_lines =
        std::fs::read(&queries_path).expect("Failed to read queries.txt");

    let queries: Vec<parser::Query> = query_lines
        .split(|&b| b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            parser::parse_query(line).expect("Failed to parse query")
        })
        .collect();

    let query_loading_time = query_start.elapsed();

    println!("Parsed {} queries", queries.len());
    println!("Query loading/parsing time: {:?}", query_loading_time);

    // Stage 2: Build the inverted index.
    let index_start = Instant::now();

    println!("Building inverted index from {}...", docs_path.display());

    // index.rs expects &str rather than &PathBuf.
    let docs_path_str = docs_path
        .to_str()
        .expect("Invalid UTF-8 document path");

    let index =
        index::build_index(docs_path_str, &queries)
            .expect("Failed to build index");

    let index_build_time = index_start.elapsed();

    println!("Documents indexed: {}", index.document_count);
    println!("Unique query terms: {}", index.terms.len());
    println!("Index building time: {:?}", index_build_time);

    // Stage 3: Execute all queries.
    let execution_start = Instant::now();

    println!("Executing queries...");

    let mut output = String::new();

    for (i, query) in queries.iter().enumerate() {
        let results = query::execute_query(&index, query);

        output.push_str(&query::format_result(&results));
        output.push('\n');

        if (i + 1) % 1000 == 0 {
            println!("Executed {}/{} queries", i + 1, queries.len());
        }
    }

    let query_execution_time = execution_start.elapsed();

    println!("Query execution time: {:?}", query_execution_time);

    // Stage 4: Write results.
    let write_start = Instant::now();

    std::fs::create_dir_all(&output_dir)
        .expect("Failed to create output directory");

    std::fs::write(&output_path, output)
        .expect("Failed to write results.txt");

    let output_writing_time = write_start.elapsed();
    let total_time = total_start.elapsed();

    println!("Output writing time: {:?}", output_writing_time);
    println!("All queries executed successfully.");
    println!("Output saved to: {}", output_path.display());

    println!("\n========== PERFORMANCE SUMMARY ==========");
    println!("Query loading/parsing : {:?}", query_loading_time);
    println!("Index building        : {:?}", index_build_time);
    println!("Query execution       : {:?}", query_execution_time);
    println!("Output writing        : {:?}", output_writing_time);
    println!("Total runtime         : {:?}", total_time);
    println!("=========================================");
}

// Used by tokenizer unit tests.
#[cfg(test)]
mod tests {
    use super::tokenize;

    #[test]
    fn tokenizes_ascii_and_lowercases() {
        assert_eq!(
            tokenize(b"Hello, WORLD! 123"),
            vec![
                b"hello".to_vec(),
                b"world".to_vec(),
                b"123".to_vec()
            ]
        );
    }

    #[test]
    fn treats_non_ascii_bytes_as_separators() {
        assert_eq!(
            tokenize("café".as_bytes()),
            vec![b"caf".to_vec()]
        );
    }
}
