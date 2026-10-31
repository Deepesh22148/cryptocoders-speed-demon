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
using u8 = uint8_t;

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
    u64 settled = 0;
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
        // Invariant: both heap tops are valid (a side's labels only change when that side expands).
        if (S.heap[0].empty() || S.heap[1].empty()) break;
        if ((u64)S.heap[0].top().k + (u64)S.heap[1].top().k >= mu) break;

        int side = S.heap[0].size() <= S.heap[1].size() ? 0 : 1;
        auto& H = S.heap[side];
        auto e = H.pop();
        ++S.settled;
        u32 u = e.v;
        D d = e.k;
        const u32* off = offs[side];
        const Edge* adj = adjs[side];
        D* mine = S.lab[side];
        const D* other = S.lab[side ^ 1];
        auto& T = S.touched[side];
        u32 i0 = off[u], i1 = off[u + 1];
        for (u32 i = i0; i < i1; ++i) {  // overlap the cache misses of all neighbours
            __builtin_prefetch(&mine[adj[i].to]);
            __builtin_prefetch(&other[adj[i].to]);
        }
        for (u32 i = i0; i < i1; ++i) {
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
        while (!H.empty() && mine[H.top().v] < (D)(H.top().k + 1)) H.pop();  // restore invariant
    }
    // reset only what we touched
    for (int side = 0; side < 2; ++side) {
        D* L = S.lab[side];
        for (u32 v : S.touched[side]) L[v] = 0;
        S.touched[side].clear();
    }
    return mu == INF ? -1 : (i64)mu;
}

static std::atomic<u64> g_settled{0}, g_maxPerThread{0};

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
            g_settled += S.settled;
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

    std::vector<std::vector<E3>> parts;
    u64 sumW = 0, maxW = 0;
    size_t n = 0, m = 0;

    auto parseSpan = [](const char* a, const char* b, std::vector<E3>& vec,
                        u64& cnt, u64& s, u64& mx, u64 limit) {
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
    };

    bool ok = false;
    {   // ---- fast path: header via pread, then T threads pread+parse their own byte ranges ----
        int fd = open("/data/graph.txt", O_RDONLY);
        if (fd < 0) { perror("/data/graph.txt"); return 1; }
        struct stat st;
        fstat(fd, &st);
        const u64 fsz = (u64)st.st_size;
        char hb[256];
        ssize_t hn = pread(fd, hb, sizeof(hb), 0);
        ssize_t nl = -1;
        for (ssize_t i = 0; i < hn; ++i) if (hb[i] == '\n') { nl = i; break; }
        if (hn > 0 && nl >= 0) {
            const char* hp = hb;
            n = readNum(hp, hb + nl);
            m = readNum(hp, hb + nl);
            const u64 start0 = (u64)nl + 1;
            const unsigned T = NT;
            std::vector<u64> bnd(T + 1);
            for (unsigned k = 0; k <= T; ++k) {
                if (k == 0) { bnd[k] = start0; continue; }
                if (k == T) { bnd[k] = fsz; continue; }
                u64 raw = start0 + (fsz - start0) / T * k;
                if (raw < bnd[k - 1]) raw = bnd[k - 1];
                u64 pos = raw ? raw - 1 : 0;  // smallest b >= raw with byte[b-1] == '\n'
                u64 b = fsz;
                char tmp[4096];
                bool found = false;
                while (pos < fsz && !found) {
                    ssize_t r = pread(fd, tmp, sizeof(tmp), pos);
                    if (r <= 0) break;
                    for (ssize_t j = 0; j < r; ++j)
                        if (tmp[j] == '\n') { b = pos + j + 1; found = true; break; }
                    pos += r;
                }
                bnd[k] = b;
            }
            parts.assign(T, {});
            std::vector<u64> parsed(T, 0), sw(T, 0), mw(T, 0);
            std::atomic<bool> bad{false};
            constexpr size_t BLK = 1 << 20, MAXLINE = 512;
            parallelRun(T, [&](unsigned k) {
                u64 off = bnd[k], end = bnd[k + 1];
                auto& vec = parts[k];
                vec.reserve((m / T) + (m / T) / 8 + 1024);
                std::vector<char> buf(BLK + MAXLINE);
                size_t carry = 0;
                u64 cnt = 0, s = 0, mx = 0;
                for (;;) {
                    size_t want = (size_t)std::min<u64>(BLK, end - off);
                    ssize_t r = want ? pread(fd, buf.data() + carry, want, off) : 0;
                    if (r < 0) { bad = true; break; }
                    off += r;
                    size_t total = carry + (size_t)r;
                    bool last = (r == 0) || off >= end;
                    size_t limit = total;
                    if (!last) {
                        limit = 0;
                        for (size_t i = total; i > 0; --i) if (buf[i - 1] == '\n') { limit = i; break; }
                        if (limit == 0 || total - limit > MAXLINE) { bad = true; break; }
                    }
                    parseSpan(buf.data(), buf.data() + limit, vec, cnt, s, mx, ~0ULL);
                    carry = total - limit;
                    memmove(buf.data(), buf.data() + limit, carry);
                    if (last) break;
                }
                parsed[k] = cnt; sw[k] = s; mw[k] = mx;
            });
            u64 tot = 0;
            for (unsigned k = 0; k < T; ++k) { tot += parsed[k]; sumW += sw[k]; maxW = std::max(maxW, mw[k]); }
            ok = !bad && tot == m;
        }
        close(fd);
    }
    if (!ok) {  // unexpected layout: plain sequential mmap parse (the v3/v4 behaviour)
        size_t glen;
        const char* g = mapFile("/data/graph.txt", glen, false);
        const char* p = g;
        const char* gend = g + glen;
        n = readNum(p, gend); m = readNum(p, gend);
        parts.assign(1, {});
        parts[0].reserve(m);
        u64 cnt = 0, s = 0, mx = 0;
        parseSpan(p, gend, parts[0], cnt, s, mx, m);
        sumW = s; maxW = mx;
        munmap((void*)g, glen);
    }
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

    // ---- locality renumbering (BFS order) when node ids look scattered ----
    std::vector<u32> newId;  // empty if not renumbered
    {
        // sample edges: what fraction connect nearby ids?
        u64 nearCnt = 0, totCnt = 0;
        size_t stride = std::max<size_t>(1, n / 200000);
        for (size_t u = 0; u < n; u += stride)
            for (u32 i = offs[0][u]; i < offs[0][u + 1]; ++i) {
                u32 v = adjs[0][i].to;
                ++totCnt;
                if ((u > v ? u - v : v - u) <= 4096) ++nearCnt;
            }
        double nearFrac = totCnt ? (double)nearCnt / totCnt : 1.0;
        bool doIt = totCnt && nearFrac < 0.6;
        if (const char* ev = getenv("RENUM")) doIt = atoi(ev) != 0;
        if (TIMING) fprintf(stderr, "          id locality: %.1f%% of edges within 4096 ids -> renumber=%d\n", nearFrac * 100, (int)doIt);
        if (doIt && n > 1) {
            // Parallel region-growing BFS: each thread grows a BFS order from its own seed,
            // claiming nodes with a byte flag; leftovers (other components) are swept up afterwards.
            u8* vis = (u8*)calloc(n, 1);
            auto claim = [&](u32 v) -> bool {
                return vis[v] == 0 && __atomic_exchange_n(&vis[v], (u8)1, __ATOMIC_RELAXED) == 0;
            };
            auto grow = [&](std::vector<u32>& L) {
                size_t head = 0;
                while (head < L.size()) {
                    u32 u = L[head];
                    if (head + 16 < L.size()) {
                        u32 x = L[head + 16];
                        __builtin_prefetch(&offs[0][x]); __builtin_prefetch(&offs[1][x]);
                    }
                    if (head + 8 < L.size()) {
                        u32 y = L[head + 8];
                        __builtin_prefetch(&adjs[0][offs[0][y]]); __builtin_prefetch(&adjs[1][offs[1][y]]);
                    }
                    ++head;
                    for (int d = 0; d < 2; ++d)
                        for (u32 i = offs[d][u], ie = offs[d][u + 1]; i < ie; ++i) {
                            u32 v = adjs[d][i].to;
                            if (claim(v)) L.push_back(v);
                        }
                }
            };
            std::vector<std::vector<u32>> lists(NT + 1);
            parallelRun(NT, [&](unsigned k) {
                auto& L = lists[k];
                L.reserve(n / NT + n / NT / 2 + 1024);
                u32 seed = (u32)((u64)n * k / NT);
                if (claim(seed)) { L.push_back(seed); grow(L); }
            });
            {   // other components / anything the seeds could not reach
                auto& L = lists[NT];
                for (u32 root = 0; root < n; ++root)
                    if (claim(root)) { L.push_back(root); grow(L); }
            }
            free(vis);
            u32* order = (u32*)malloc(n * sizeof(u32));
            {
                size_t pos = 0;
                for (auto& L : lists) { memcpy(order + pos, L.data(), L.size() * sizeof(u32)); pos += L.size(); }
            }
            lists.clear();
            lap("bfs order");
            newId.resize(n);
            const size_t blk = (n + NT - 1) / NT;
            parallelRun(NT, [&](unsigned k) {
                size_t lo = std::min<size_t>(n, k * blk), hi = std::min<size_t>(n, lo + blk);
                for (size_t i = lo; i < hi; ++i) newId[order[i]] = (u32)i;
            });
            for (int d = 0; d < 2; ++d) {
                u32* noff = (u32*)malloc((n + 1) * sizeof(u32));
                Edge* nadj = (Edge*)malloc((E ? E : 1) * sizeof(Edge));
                noff[0] = 0;
                parallelRun(NT, [&](unsigned k) {
                    size_t lo = std::min<size_t>(n, k * blk), hi = std::min<size_t>(n, lo + blk);
                    for (size_t i = lo; i < hi; ++i) { u32 o = order[i]; noff[i + 1] = offs[d][o + 1] - offs[d][o]; }
                });
                for (size_t i = 0; i < n; ++i) noff[i + 1] += noff[i];
                parallelRun(NT, [&](unsigned k) {
                    size_t lo = std::min<size_t>(n, k * blk), hi = std::min<size_t>(n, lo + blk);
                    for (size_t i = lo; i < hi; ++i) {
                        u32 o = order[i];
                        u32 w0 = noff[i];
                        for (u32 j = offs[d][o], je = offs[d][o + 1]; j < je; ++j, ++w0)
                            nadj[w0] = {newId[adjs[d][j].to], adjs[d][j].w};
                    }
                });
                free(offs[d]); free(adjs[d]);
                offs[d] = noff; adjs[d] = nadj;
            }
            free(order);
            lap("relabel");
        }
    }

    // ---- queries ----
    size_t qlen;
    const char* q = mapFile("/data/queries.txt", qlen, true);
    const char* qend = q + qlen;
    std::vector<std::pair<u32, u32>> queries;
    while (true) {
        while (q < qend && (unsigned)(*q - '0') > 9) ++q;
        if (q >= qend) break;
        u32 s = (u32)readNum(q, qend), t = (u32)readNum(q, qend);
        if (!newId.empty()) { s = newId[s]; t = newId[t]; }
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
    if (TIMING && !queries.empty())
        fprintf(stderr, "          settled nodes: total %llu, avg %.0f per query, %s labels\n",
                (unsigned long long)g_settled.load(), (double)g_settled.load() / queries.size(),
                bound < 0xFFFFFFF0ULL ? "32-bit" : "64-bit");

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
    if (TIMING) fprintf(stderr, "          threads: %u\n", NT);
    _exit(0);  // skip freeing ~hundreds of MB at teardown
}