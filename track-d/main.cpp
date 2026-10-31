#include <algorithm>
#include <atomic>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
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

// Forward CSR (dir 0) and reverse CSR (dir 1).
static std::vector<u32> offs[2];
static std::vector<Edge> adjs[2];

// ---- 4-ary min-heap with lazy deletion ----
struct Ent { u64 k; u32 v; };

struct Heap4 {
    std::vector<Ent> a;
    bool empty() const { return a.empty(); }
    size_t size() const { return a.size(); }
    void clear() { a.clear(); }
    const Ent& top() const { return a[0]; }
    void push(Ent e) {
        a.push_back(e);
        size_t i = a.size() - 1;
        while (i > 0) {
            size_t p = (i - 1) >> 2;
            if (a[p].k <= e.k) break;
            a[i] = a[p];
            i = p;
        }
        a[i] = e;
    }
    Ent pop() {
        Ent res = a[0];
        Ent e = a.back();
        a.pop_back();
        size_t n = a.size();
        if (n) {
            size_t i = 0;
            for (;;) {
                size_t c = i * 4 + 1;
                if (c >= n) break;
                size_t best = c, end = std::min(c + 4, n);
                for (size_t j = c + 1; j < end; ++j)
                    if (a[j].k < a[best].k) best = j;
                if (a[best].k >= e.k) break;
                a[i] = a[best];
                i = best;
            }
            a[i] = e;
        }
        return res;
    }
};

// dist + stamp packed together: one cache line touch per node visit.
struct __attribute__((packed)) Lab { u64 d; u32 stamp; };

struct Scratch {
    Lab* lab[2];
    u32 epoch = 0;
    Heap4 heap[2];
    explicit Scratch(size_t n) {
        for (int s = 0; s < 2; ++s) lab[s] = (Lab*)calloc(n, sizeof(Lab));
    }
};

static std::vector<u32> indeg;

static i64 solve(Scratch& S, u32 s, u32 t) {
    if (s == t) return 0;
    if (indeg[t] == 0 || offs[0][s] == offs[0][s + 1]) return -1;

    const u64 INF = ~0ULL;
    u64 mu = INF;
    u32 ep = ++S.epoch;
    S.heap[0].clear(); S.heap[1].clear();
    S.lab[0][s] = {0, ep}; S.heap[0].push({0, s});
    S.lab[1][t] = {0, ep}; S.heap[1].push({0, t});

    for (;;) {
        // drop stale tops so the stopping bound is tight
        for (int side = 0; side < 2; ++side) {
            Heap4& H = S.heap[side];
            while (!H.empty() && H.top().k > S.lab[side][H.top().v].d) H.pop();
        }
        if (S.heap[0].empty() || S.heap[1].empty()) break;
        if (S.heap[0].top().k + S.heap[1].top().k >= mu) break;

        int side = S.heap[0].size() <= S.heap[1].size() ? 0 : 1;
        Ent e = S.heap[side].pop();
        u32 u = e.v;
        u64 d = e.k;
        const u32* off = offs[side].data();
        const Edge* adj = adjs[side].data();
        Lab* mine = S.lab[side];
        Lab* other = S.lab[side ^ 1];
        Heap4& H = S.heap[side];
        for (u32 i = off[u], iend = off[u + 1]; i < iend; ++i) {
            u32 v = adj[i].to;
            u64 nd = d + adj[i].w;
            if (mine[v].stamp != ep || nd < mine[v].d) {
                mine[v].stamp = ep;
                mine[v].d = nd;
                H.push({nd, v});
            }
            if (other[v].stamp == ep) {
                u64 cand = nd + other[v].d;
                if (cand < mu) mu = cand;
            }
        }
    }
    return mu == INF ? -1 : (i64)mu;
}

int main() {
    size_t glen;
    const char* g = mapFile("/data/graph.txt", glen);
    const char* p = g;
    const char* gend = g + glen;
    size_t n = readNum(p, gend), m = readNum(p, gend);

    std::vector<u32> eu, ev, ew;
    eu.reserve(m); ev.reserve(m); ew.reserve(m);
    offs[0].assign(n + 1, 0);
    offs[1].assign(n + 1, 0);
    indeg.assign(n, 0);
    for (size_t i = 0; i < m; ++i) {
        u32 u = (u32)readNum(p, gend), v = (u32)readNum(p, gend), w = (u32)readNum(p, gend);
        if (u == v) continue;  // a self-loop never shortens a path
        eu.push_back(u); ev.push_back(v); ew.push_back(w);
        ++offs[0][u + 1];
        ++offs[1][v + 1];
        ++indeg[v];
    }
    for (int s = 0; s < 2; ++s)
        for (size_t i = 0; i < n; ++i) offs[s][i + 1] += offs[s][i];

    size_t E = eu.size();
    adjs[0].resize(E);
    adjs[1].resize(E);
    {
        std::vector<u32> pf(offs[0].begin(), offs[0].end() - 1);
        std::vector<u32> pb(offs[1].begin(), offs[1].end() - 1);
        for (size_t i = 0; i < E; ++i) {
            adjs[0][pf[eu[i]]++] = {ev[i], ew[i]};
            adjs[1][pb[ev[i]]++] = {eu[i], ew[i]};
        }
    }
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
    unsigned nthreads = 12;  // tune to the scorer's core count
    if (nthreads > queries.size()) nthreads = (unsigned)std::max<size_t>(1, queries.size());
    std::vector<std::thread> pool;
    for (unsigned k = 0; k < nthreads; ++k) {
        pool.emplace_back([&] {
            Scratch S(n);
            constexpr size_t CHUNK = 16;  // fewer atomic hits
            size_t i;
            while ((i = next.fetch_add(CHUNK)) < queries.size()) {
                size_t hi = std::min(i + CHUNK, queries.size());
                for (; i < hi; ++i) ans[i] = solve(S, queries[i].first, queries[i].second);
            }
        });
    }
    for (auto& th : pool) th.join();

    std::string out;
    out.reserve(ans.size() * 12);
    char buf[24];
    for (i64 a : ans) {
        if (a < 0) { out += "-1\n"; continue; }
        int len = 0;
        u64 x = (u64)a;
        do { buf[len++] = '0' + x % 10; x /= 10; } while (x);
        while (len) out += buf[--len];
        out += '\n';
    }
    FILE* f = fopen("/out/distances.txt", "wb");
    if (!f) { perror("/out/distances.txt"); return 1; }
    fwrite(out.data(), 1, out.size(), f);
    fclose(f);
    return 0;
}