#include <algorithm>
#include <atomic>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <string>
#include <thread>
#include <vector>
#include <fcntl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

using u32 = uint32_t;
using u64 = uint64_t;
using i64 = int64_t;

struct Edge { u32 to, w; };

static const char* mapFile(const char* path, size_t& len) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) { perror(path); exit(1); }
    struct stat st;
    fstat(fd, &st);
    len = st.st_size;
    const char* p = nullptr;
    if (len) p = (const char*)mmap(nullptr, len, PROT_READ, MAP_PRIVATE | MAP_POPULATE, fd, 0);
    close(fd);
    return p;
}

static inline u64 readNum(const char*& p, const char* end) {
    while (p < end && (unsigned)(*p - '0') > 9) ++p;
    u64 x = 0;
    while (p < end && (unsigned)(*p - '0') <= 9) { x = x * 10 + (*p - '0'); ++p; }
    return x;
}

static std::vector<u32> off, indeg;
static std::vector<Edge> adj;

struct Scratch {
    u64* dist; u32* stamp; u32 epoch = 0;
    std::vector<std::pair<u64, u32>> heap;
    explicit Scratch(size_t n) {
        dist = (u64*)malloc(n * sizeof(u64));
        stamp = (u32*)calloc(n, sizeof(u32));
    }
};

static i64 solve(Scratch& S, u32 s, u32 t) {
    if (s == t) return 0;
    if (indeg[t] == 0 || off[s] == off[s + 1]) return -1;
    auto cmp = [](const std::pair<u64, u32>& a, const std::pair<u64, u32>& b) { return a.first > b.first; };
    ++S.epoch;
    S.heap.clear();
    S.dist[s] = 0; S.stamp[s] = S.epoch;
    S.heap.push_back({0, s});
    while (!S.heap.empty()) {
        std::pop_heap(S.heap.begin(), S.heap.end(), cmp);
        auto [d, u] = S.heap.back();
        S.heap.pop_back();
        if (d > S.dist[u]) continue;
        if (u == t) return (i64)d;
        for (u32 i = off[u]; i < off[u + 1]; ++i) {
            u32 v = adj[i].to;
            u64 nd = d + adj[i].w;
            if (S.stamp[v] != S.epoch || nd < S.dist[v]) {
                S.stamp[v] = S.epoch;
                S.dist[v] = nd;
                S.heap.push_back({nd, v});
                std::push_heap(S.heap.begin(), S.heap.end(), cmp);
            }
        }
    }
    return -1;
}

int main() {
    size_t glen;
    const char* g = mapFile("/data/graph.txt", glen);
    const char* p = g;
    const char* gend = g + glen;
    size_t n = readNum(p, gend), m = readNum(p, gend);

    std::vector<u32> eu, ev, ew;
    eu.reserve(m); ev.reserve(m); ew.reserve(m);
    off.assign(n + 1, 0);
    indeg.assign(n, 0);
    for (size_t i = 0; i < m; ++i) {
        u32 u = (u32)readNum(p, gend), v = (u32)readNum(p, gend), w = (u32)readNum(p, gend);
        if (u == v) continue;  // a self-loop never shortens a path
        eu.push_back(u); ev.push_back(v); ew.push_back(w);
        ++off[u + 1];
        ++indeg[v];
    }
    for (size_t i = 0; i < n; ++i) off[i + 1] += off[i];
    adj.resize(eu.size());
    std::vector<u32> pos(off.begin(), off.end() - 1);
    for (size_t i = 0; i < eu.size(); ++i) adj[pos[eu[i]]++] = {ev[i], ew[i]};
    std::vector<u32>().swap(eu); std::vector<u32>().swap(ev); std::vector<u32>().swap(ew);

    size_t qlen;
    const char* q = mapFile("/data/queries.txt", qlen);
    const char* qend = q + qlen;
    std::vector<std::pair<u32, u32>> queries;
    while (true) {
        while (q < qend && (unsigned)(*q - '0') > 9) ++q;
        if (q >= qend) break;
        u32 s = (u32)readNum(q, qend), t = (u32)readNum(q, qend);
        queries.push_back({s, t});
    }

    std::vector<i64> ans(queries.size());
    std::atomic<size_t> next{0};
    unsigned nthreads = 12;
    std::vector<std::thread> pool;
    for (unsigned k = 0; k < nthreads; ++k) {
        pool.emplace_back([&] {
            Scratch S(n);
            size_t i;
            while ((i = next.fetch_add(1)) < queries.size())
                ans[i] = solve(S, queries[i].first, queries[i].second);
        });
    }
    for (auto& th : pool) th.join();

    std::string out;
    for (i64 a : ans) { out += std::to_string(a); out += '\n'; }
    FILE* f = fopen("/out/distances.txt", "wb");
    if (!f) { perror("/out/distances.txt"); return 1; }
    fwrite(out.data(), 1, out.size(), f);
    fclose(f);
    return 0;
}