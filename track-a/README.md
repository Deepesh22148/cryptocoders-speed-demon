# Track A: Ingest

## Approach

The solution processes the request log using parallel chunk-based parsing and hourly aggregation. Multiple worker threads read separate file segments using `pread`, handling chunk boundaries to avoid processing incomplete records.

Each thread maintains a local endpoint dictionary to reduce synchronization overhead, while a global registry assigns endpoint IDs and maintains endpoints in bytewise sorted order for deterministic output.

Parsed records are stored in reusable fixed-size memory pages grouped by hour. Once an hour is ready for processing, the solution aggregates request counts, error counts, and latency values for each endpoint. Exact p50, p95, and p99 latency percentiles are calculated using a logarithmic histogram to narrow the search range, followed by `nth_element` within the relevant buckets. Small groups use insertion sort.

The implementation emphasizes parallelism, cache-friendly data layouts, efficient numeric parsing, reduced memory allocation, and page recycling. A safe-mode restart handles cases where the input ordering violates the assumptions used for early hourly finalization.

## Dependencies

None. Uses the C++ standard library and Linux system APIs.