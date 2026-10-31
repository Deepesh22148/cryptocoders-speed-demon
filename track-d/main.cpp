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
#include <sched.h>
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
static u32* offs[2];
static Edge* adjs[2];

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
        const u32* off = offs[side];
        const Edge* adj = adjs[side];
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
                       const std::vector<u32>& order, std::vector<i64>& ans, unsigned nthreads) {
    std::atomic<size_t> next{0};
    if (nthreads > qs.size()) nthreads = (unsigned)std::max<size_t>(1, qs.size());
    std::vector<std::thread> pool;
    for (unsigned k = 0; k < nthreads; ++k) {
        pool.emplace_back([&] {
            Scratch<D> S(n);
            constexpr size_t CHUNK = 2;
            size_t i;
            while ((i = next.fetch_add(CHUNK)) < qs.size()) {
                size_t hi = std::min(i + CHUNK, qs.size());
                for (; i < hi; ++i) {
                    u32 qi = order[i];
                    ans[qi] = solve<D>(S, qs[qi].first, qs[qi].second);
                }
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

    unsigned NT = 12;  // cap; lowered if the container exposes fewer CPUs
    {
        cpu_set_t cs;
        if (sched_getaffinity(0, sizeof(cs), &cs) == 0) {
            unsigned c = (unsigned)CPU_COUNT(&cs);
            if (c >= 1 && c < NT) NT = c;
        }
    }

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

    // ---- cache-friendly CSR build (forward + reverse) ----
    // 1) histogram edges by node-range bucket, 2) scatter into bucket order (no atomics),
    // 3) each bucket is turned into CSR by one thread, entirely inside L2 cache.
    const unsigned SH = 15;  // 32768 nodes per bucket
    unsigned T = (unsigned)parts.size();
    size_t B = (n >> SH) + 1;
    std::vector<std::vector<size_t>> pF(T, std::vector<size_t>(B, 0)), pR(T, std::vector<size_t>(B, 0));
    parallelRun(T, [&](unsigned k) {
        for (const E3& e : parts[k]) { ++pF[k][e.u >> SH]; ++pR[k][e.v >> SH]; }
    });
    std::vector<size_t> bsF(B + 1), bsR(B + 1);
    size_t E = 0;
    {
        size_t runF = 0, runR = 0;
        for (size_t b = 0; b < B; ++b) {
            bsF[b] = runF; bsR[b] = runR;
            for (unsigned k = 0; k < T; ++k) {
                size_t cf = pF[k][b], cr = pR[k][b];
                pF[k][b] = runF; pR[k][b] = runR;
                runF += cf; runR += cr;
            }
        }
        bsF[B] = runF; bsR[B] = runR;
        E = runF;
    }
    E3* tmpF = new E3[E ? E : 1];  // uninitialised on purpose; pages get touched in parallel
    E3* tmpR = new E3[E ? E : 1];
    parallelRun(T, [&](unsigned k) {
        auto& pf = pF[k];
        auto& pr = pR[k];
        for (const E3& e : parts[k]) {
            tmpF[pf[e.u >> SH]++] = e;
            tmpR[pr[e.v >> SH]++] = e;
        }
    });
    parts.clear(); parts.shrink_to_fit();

    for (int s = 0; s < 2; ++s) {
        offs[s] = (u32*)malloc((n + 1) * sizeof(u32));
        adjs[s] = (Edge*)malloc((E ? E : 1) * sizeof(Edge));
        offs[s][n] = (u32)E;
    }
    auto buildDir = [&](const E3* tmp, const std::vector<size_t>& bs, bool fwd, u32* off, Edge* adj) {
        std::atomic<size_t> nb{0};
        parallelRun(NT, [&](unsigned) {
            std::vector<u32> cnt;
            size_t b;
            while ((b = nb.fetch_add(1)) < B) {
                size_t lo = b << SH, hi = std::min(n, (b + 1) << SH);
                size_t sz = hi > lo ? hi - lo : 0;
                cnt.assign(sz, 0);
                size_t i0 = bs[b], i1 = bs[b + 1];
                if (fwd) for (size_t i = i0; i < i1; ++i) ++cnt[tmp[i].u - lo];
                else     for (size_t i = i0; i < i1; ++i) ++cnt[tmp[i].v - lo];
                u32 run = (u32)i0;
                for (size_t j = 0; j < sz; ++j) { u32 c = cnt[j]; off[lo + j] = run; cnt[j] = run; run += c; }
                if (fwd) for (size_t i = i0; i < i1; ++i) { const E3& e = tmp[i]; adj[cnt[e.u - lo]++] = {e.v, e.w}; }
                else     for (size_t i = i0; i < i1; ++i) { const E3& e = tmp[i]; adj[cnt[e.v - lo]++] = {e.u, e.w}; }
            }
        });
    };
    buildDir(tmpF, bsF, true, offs[0], adjs[0]);
    delete[] tmpF;
    buildDir(tmpR, bsR, false, offs[1], adjs[1]);
    delete[] tmpR;
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
    std::vector<u32> order(queries.size());
    for (size_t i = 0; i < order.size(); ++i) order[i] = (u32)i;
    std::sort(order.begin(), order.end(), [&](u32 a, u32 b) { return queries[a].first < queries[b].first; });
    if (bound < 0xFFFFFFF0ULL) runQueries<u32>(n, queries, order, ans, NT);
    else runQueries<u64>(n, queries, order, ans, NT);
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
    _exit(0);  // skip freeing ~hundreds of MB at teardown
}