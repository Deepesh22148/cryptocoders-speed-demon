# Track D: Traverse

## Approach

The solution constructs compact adjacency lists for a directed, weighted graph and answers shortest-path queries using bidirectional Dijkstra's algorithm.

The graph input is parsed in parallel, and edges are reorganized into forward and reverse adjacency lists using counting-based partitioning and prefix sums. This enables efficient graph traversal from both the source and destination.

For each query, the algorithm expands the search frontier with the smaller priority queue, maintains the best known path connecting the two searches, and terminates when further exploration cannot improve the current shortest distance.

A custom bucket-based priority queue reduces the overhead of conventional heap operations. The implementation also uses compact integer-based storage, lazy removal of outdated queue entries, prefetching, and parallel graph construction.

When vertex numbering poorly reflects graph connectivity, the solution conditionally reorders vertices using parallel graph traversal to improve memory locality. It uses 32-bit distance labels when possible and retries with 64-bit labels if overflow is detected.

These optimizations aim to reduce preprocessing time, memory usage, and query latency while preserving exact shortest-path distances.

## Dependencies

None. Uses the C++ standard library and Linux system APIs.