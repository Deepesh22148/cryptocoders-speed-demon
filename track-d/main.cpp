#include <algorithm>
#include <atomic>
#include <chrono>
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
struct E3 { u32 u, v, w; };

static const char* mapFile(const char* path, size_t& len, bool populate) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) { perror(path); exit(1); }
    struct stat st;
    fstat(fd, &st);
    len = st.st_size;
    const char* p = nullptr;
    if (len) {
        p = (const char*)mmap(nullptr, len, PROT_READ,
                              MAP_PRIVATE | (populate ? MAP_POPULATE : 0), fd, 0);
        if (p == MAP_FAILED) { perror("mmap"); exit(1); }
    }
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
template <class D>
struct Heap4 {
    struct Ent { D k; u32 v; };
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

// Per-thread scratch. Label code = dist + 1, and 0 means "unlabeled".
// calloc gives lazily-zeroed pages, and touched nodes are reset after each query,
// so there are no stamps and no big memset. 4 bytes/node/direction when D = u32.
template <class D>
struct Scratch {
    D* lab[2];
    std::vector<u32> touched[2];
    Heap4<D> heap[2];
    explicit Scratch(size_t n) {
        for (int s = 0; s < 2; ++s) lab[s] = (D*)calloc(n, sizeof(D));
    }
    ~Scratch() { free(lab[0]); free(lab[1]); }
};

template <class D>
static i64 solve(Scratch<D>& S, u32 s, u32 t) {
    if (s == t) return 0;
    // no out-edges at s, or no in-edges at t -> unreachable
    if (offs[0][s] == offs[0][s + 1] || offs[1][t] == offs[1][t + 1]) return -1;

    const u64 INF = ~0ULL;
    u64 mu = INF;
    S.heap[0].clear(); S.heap[1].clear();
    S.lab[0][s] = 1; S.touched[0].push_back(s); S.heap[0].push({0, s});
    S.lab[1][t] = 1; S.touched[1].push_back(t); S.heap[1].push({0, t});

    for (;;) {
        // drop stale tops so the stopping bound is tight
        for (int side = 0; side < 2; ++side) {
            auto& H = S.heap[side];
            const D* L = S.lab[side];
            while (!H.empty() && L[H.top().v] < (D)(H.top().k + 1)) H.pop();
        }
        if (S.heap[0].empty() || S.heap[1].empty()) break;
        if ((u64)S.heap[0].top().k + (u64)S.heap[1].top().k >= mu) break;

        int side = S.heap[0].size() <= S.heap[1].size() ? 0 : 1;
        auto e = S.heap[side].pop();
        u32 u = e.v;
        D d = e.k;
        const u32* off = offs[side].data();
        const Edge* adj = adjs[side].data();
        D* mine = S.lab[side];
        const D* other = S.lab[side ^ 1];
        auto& H = S.heap[side];
        auto& T = S.touched[side];
        for (u32 i = off[u], iend = off[u + 1]; i < iend; ++i) {
            u32 v = adj[i].to;
            D nd = d + (D)adj[i].w;
            D c = nd + 1;
            D cur = mine[v];
            if (cur == 0 || c < cur) {
                if (cur == 0) T.push_back(v);
                mine[v] = c;
                H.push({nd, v});
            }
            D o = other[v];
            if (o) {
                u64 cand = (u64)nd + (u64)(o - 1);
                if (cand < mu) mu = cand;
            }
        }
    }
    // reset only what we touched
    for (int side = 0; side < 2; ++side) {
        D* L = S.lab[side];
        for (u32 v : S.touched[side]) L[v] = 0;
        S.touched[side].clear();
    }
    return mu == INF ? -1 : (i64)mu;
}

template <class D>
static void runQueries(size_t n, const std::vector<std::pair<u32, u32>>& qs,
                       std::vector<i64>& ans, unsigned nthreads) {
    std::atomic<size_t> next{0};
    if (nthreads > qs.size()) nthreads = (unsigned)std::max<size_t>(1, qs.size());
    std::vector<std::thread> pool;
    for (unsigned k = 0; k < nthreads; ++k) {
        pool.emplace_back([&] {
            Scratch<D> S(n);
            constexpr size_t CHUNK = 4;
            size_t i;
            while ((i = next.fetch_add(CHUNK)) < qs.size()) {
                size_t hi = std::min(i + CHUNK, qs.size());
                for (; i < hi; ++i) ans[i] = solve<D>(S, qs[i].first, qs[i].second);
            }
        });
    }
    for (auto& th : pool) th.join();
}

static std::chrono::steady_clock::time_point T0;
static bool TIMING = false;
static void lap(const char* name) {
    if (!TIMING) return;
    double ms = std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - T0).count();
    fprintf(stderr, "[%8.2f ms] %s\n", ms, name);
}

template <class F>
static void parallelRun(unsigned T, F&& f) {
    if (T == 1) { f(0u); return; }
    std::vector<std::thread> th;
    for (unsigned k = 0; k < T; ++k) th.emplace_back([&f, k] { f(k); });
    for (auto& t : th) t.join();
}

int main() {
    T0 = std::chrono::steady_clock::now();
    TIMING = getenv("TIMING") != nullptr;

    const unsigned NT = 12;  // tune to the scorer's core count

    size_t glen;
    const char* g = mapFile("/data/graph.txt", glen, false);
    const char* p = g;
    const char* gend = g + glen;
    size_t n = readNum(p, gend), m = readNum(p, gend);

    // ---- parallel parse into per-thread edge lists ----
    std::vector<std::vector<E3>> parts;
    u64 sumW = 0, maxW = 0;

    auto parseWith = [&](unsigned T, u64 limit) -> u64 {
        parts.assign(T, {});
        std::vector<const char*> bnd(T + 1);
        size_t len = gend - p;
        for (unsigned k = 0; k <= T; ++k) {
            const char* b = (k == T) ? gend : p + (len / T) * k;
            if (k > 0 && k < T) {
                while (b < gend && b[-1] != '\n') ++b;  // start on a line boundary
            }
            bnd[k] = b;
        }
        std::vector<u64> parsed(T, 0), sw(T, 0), mw(T, 0);
        parallelRun(T, [&](unsigned k) {
            const char* a = bnd[k];
            const char* b = bnd[k + 1];
            auto& vec = parts[k];
            vec.reserve((m / T) + (m / T) / 8 + 1024);
            u64 cnt = 0, s = 0, mx = 0;
            while (cnt < limit) {
                while (a < b && (unsigned)(*a - '0') > 9) ++a;
                if (a >= b) break;
                u32 u = (u32)readNum(a, b), v = (u32)readNum(a, b), w = (u32)readNum(a, b);
                ++cnt;
                if (u == v) continue;  // a self-loop never shortens a path
                vec.push_back({u, v, w});
                s += w;
                if (w > mx) mx = w;
            }
            parsed[k] = cnt; sw[k] = s; mw[k] = mx;
        });
        u64 tot = 0;
        sumW = 0; maxW = 0;
        for (unsigned k = 0; k < T; ++k) {
            tot += parsed[k]; sumW += sw[k]; maxW = std::max(maxW, mw[k]);
        }
        return tot;
    };

    u64 got = parseWith(NT, ~0ULL);
    if (got != m) {  // unexpected layout: fall back to a plain sequential parse
        parseWith(1, m);
    }
    munmap((void*)g, glen);
    lap("parse");

    // ---- parallel CSR build (forward + reverse) ----
    unsigned T = (unsigned)parts.size();
    offs[0].assign(n + 1, 0);
    offs[1].assign(n + 1, 0);
    parallelRun(T, [&](unsigned k) {
        for (const E3& e : parts[k]) {
            __atomic_fetch_add(&offs[0][e.u + 1], 1u, __ATOMIC_RELAXED);
            __atomic_fetch_add(&offs[1][e.v + 1], 1u, __ATOMIC_RELAXED);
        }
    });
    for (int s = 0; s < 2; ++s)
        for (size_t i = 0; i < n; ++i) offs[s][i + 1] += offs[s][i];

    size_t E = offs[0][n];
    adjs[0].resize(E);
    adjs[1].resize(E);
    {
        std::vector<u32> pf(offs[0].begin(), offs[0].end() - 1);
        std::vector<u32> pb(offs[1].begin(), offs[1].end() - 1);
        parallelRun(T, [&](unsigned k) {
            for (const E3& e : parts[k]) {
                u32 i = __atomic_fetch_add(&pf[e.u], 1u, __ATOMIC_RELAXED);
                adjs[0][i] = {e.v, e.w};
                u32 j = __atomic_fetch_add(&pb[e.v], 1u, __ATOMIC_RELAXED);
                adjs[1][j] = {e.u, e.w};
            }
        });
    }
    parts.clear(); parts.shrink_to_fit();
    lap("build");

    // ---- queries ----
    size_t qlen;
    const char* q = mapFile("/data/queries.txt", qlen, true);
    const char* qend = q + qlen;
    std::vector<std::pair<u32, u32>> queries;
    while (true) {
        while (q < qend && (unsigned)(*q - '0') > 9) ++q;
        if (q >= qend) break;
        u32 s = (u32)readNum(q, qend), t = (u32)readNum(q, qend);
        queries.push_back({s, t});
    }
    lap("queries read");

    // Any walk the search can label is at most min(sum of all weights, maxW * n).
    // If that fits in 32 bits (with room for the +1 label code), use 32-bit labels.
    u64 bound = std::min<u64>(sumW, maxW * (u64)n);
    std::vector<i64> ans(queries.size());
    if (bound < 0xFFFFFFF0ULL) runQueries<u32>(n, queries, ans, NT);
    else runQueries<u64>(n, queries, ans, NT);
    lap("solve");

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
    lap("write");
    return 0;
}