#!/usr/bin/env python3
"""Speed Demon 2026, Track D (Traverse): the naive reference.

    python3.13 reference/d_traverse.py [--data /data] [--out /out]

Reads graph.txt and queries.txt from the data directory and writes distances.txt
to the output directory: for each query, in input order, the length of the
shortest directed path from s to t, or -1 if t cannot be reached from s.
specs/d-traverse.md is the contract; this file is one correct way to meet it.

The approach is the textbook one: load the edges into an adjacency list of
(head, weight) pairs, then run a fresh Dijkstra search with a binary heap for
every query, stopping as soon as the target is settled.
"""

import argparse
import heapq
import os


def read_graph(path):
    with open(path, "rb") as f:
        tok = f.read().split()
    n, m = int(tok[0]), int(tok[1])
    vals = list(map(int, tok[2:2 + 3 * m]))
    adj = [[] for _ in range(n)]
    indeg = [0] * n
    for u, v, w in zip(vals[0::3], vals[1::3], vals[2::3]):
        adj[u].append((v, w))
        indeg[v] += 1
    return adj, indeg

def answer(adj, indeg, s, t):
    if s == t:
        return 0
    if indeg[t] == 0 or not adj[s]:
        return -1
    return shortest_path(adj, s, t)


def read_queries(path):
    with open(path, encoding="ascii") as f:
        return [tuple(map(int, line.split())) for line in f]


def shortest_path(adj, s, t):
    """Dijkstra from s, stopping when t is settled; -1 if t is never reached.

    Parallel edges need no special handling: each one is relaxed, and only the
    lightest can improve a distance. Weight-0 edges are fine because Dijkstra only
    needs weights to be non-negative.
    """
    dist = {s: 0}
    heap = [(0, s)]
    while heap:
        d, u = heapq.heappop(heap)
        if d > dist[u]:
            continue  # stale entry: u was already settled at a shorter distance
        if u == t:
            return d
        for v, w in adj[u]:
            nd = d + w
            if v not in dist or nd < dist[v]:
                dist[v] = nd
                heapq.heappush(heap, (nd, v))
    return -1


def main(data_dir, out_dir):
    adj, indeg = read_graph(os.path.join(data_dir, "graph.txt"))
    queries = read_queries(os.path.join(data_dir, "queries.txt"))
    cache = {}
    answers = []
    for s, t in queries:
        key = (s, t)
        if key not in cache:
            cache[key] = answer(adj, indeg, s, t)
        answers.append(cache[key])
    with open(os.path.join(out_dir, "distances.txt"), "w", encoding="ascii", newline="\n") as f:
        f.write("".join(f"{d}\n" for d in answers))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Track D (Traverse) naive reference")
    parser.add_argument("--data", default="/data")
    parser.add_argument("--out", default="/out")
    args = parser.parse_args()
    main(args.data, args.out)
