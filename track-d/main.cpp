#include <bits/stdc++.h>
#include <fcntl.h>
#include <sched.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>
using namespace std;
typedef uint32_t u32; typedef uint64_t u64;
#define F(i,a,b) for(size_t i=(a);i<(b);++i)

struct Edge{u32 to,w;};
struct E3{u32 a[3];};
struct Ent{u64 k;u32 v;};
static u32* off[2]; static Edge* adj[2];

static const u64 INF = (1ULL << 40) - 1;

static inline u64 num(const char*&p, const char*e){
  while(p < e && (unsigned)(*p - '0') > 9) ++p;
  u64 x = 0; while(p < e && (unsigned)(*p - '0') <= 9) x = x * 10 + (*p++ - '0');
  return x;
}

struct Heap{
  vector<Ent> a;
  inline void push(Ent e){
    size_t i = a.size(); a.push_back(e);
    while(i){
      size_t p = (i - 1) >> 2;
      if(a[p].k <= e.k) break;
      a[i] = a[p]; i = p;
    }
    a[i] = e;
  }
  inline void pop(){
    Ent e = a.back(); a.pop_back();
    size_t n = a.size(), i = 0;
    if(!n) return;
    for(size_t c; (c = (i << 2) + 1) < n; ){
      size_t b = c, lim = min(c + 4, n);
      for(size_t j = c + 1; j < lim; ++j) if(a[j].k < a[b].k) b = j;
      if(a[b].k >= e.k) break;
      a[i] = a[b]; i = b;
    }
    a[i] = e;
  }
};

struct Sc{
  u64* D[2];
  u64 ep;
  Heap h[2];
};

static inline u64 get_d(u64 val, u64 ep){
  return (val >> 40 == ep) ? (val & INF) : INF;
}

static long long solve(Sc& S, u32 s, u32 t){
  if(s == t) return 0;
  if(off[0][s] == off[0][s+1] || off[1][t] == off[1][t+1]) return -1;

  u64 ep = ++S.ep, mu = ~0ULL;
  u32 st[2] = {s, t};

  for(int d = 0; d < 2; ++d){
    S.h[d].a.clear();
    S.D[d][st[d]] = ep << 40;
    S.h[d].push({0, st[d]});
  }

  while(!S.h[0].a.empty() && !S.h[1].a.empty() && S.h[0].a[0].k + S.h[1].a[0].k < mu){
    int d = S.h[0].a.size() <= S.h[1].a.size() ? 0 : 1;
    Heap& h = S.h[d]; Ent e = h.a[0]; h.pop();
    u64* me = S.D[d]; u64* ot = S.D[d^1]; const Edge* A = adj[d];

    if(e.k > get_d(me[e.v], ep)) continue;

    u32 i0 = off[d][e.v], i1 = off[d][e.v+1];
    for(u32 i = i0; i < i1; ++i){
      __builtin_prefetch(me + A[i].to);
      __builtin_prefetch(ot + A[i].to);
    }

    for(u32 i = i0; i < i1; ++i){
      u32 v = A[i].to;
      u64 nd = e.k + A[i].w;
      if(nd < get_d(me[v], ep)){
        me[v] = (ep << 40) | nd;
        h.push({nd, v});
      }
      u64 od = get_d(ot[v], ep);
      if(od != INF){
        mu = min(mu, nd + od);
      }
    }
    while(!h.a.empty() && h.a[0].k > get_d(me[h.a[0].v], ep)) h.pop();
  }

  return mu == ~0ULL ? -1 : (long long)mu;
}

int main(){
  unsigned NT = 12;
  {cpu_set_t c; if(!sched_getaffinity(0, sizeof c, &c)) NT = min<int>(NT, max(1, CPU_COUNT(&c)));}
  auto par = [&](size_t T, auto f){vector<thread> v; F(k,0,T) v.emplace_back(f,(unsigned)k); for(auto& t:v) t.join();};
  auto dyn = [&](size_t N, auto f){atomic<size_t> c{0}; par(NT, [&](unsigned){size_t k; while((k=c++)<N) f(k);});};

  // ---- Parse Graph ----
  int fd = open("/data/graph.txt", O_RDONLY);
  struct stat sb; fstat(fd, &sb);
  u64 fz = sb.st_size;
  const char* base = (char*)mmap(NULL, fz, PROT_READ, MAP_SHARED | MAP_POPULATE, fd, 0);
  close(fd);

  size_t n = 0, NC = max<size_t>(NT, fz >> 20);
  vector<vector<E3>> P(NC);

  dyn(NC, [&](size_t k){
    u64 r0 = fz / NC * k, r1 = (k + 1 == NC) ? fz : fz / NC * (k + 1);
    const char* a = base + r0;
    const char* z = base + r1;
    if(k == 0){
      const char* h = a;
      n = num(h, base + min<u64>(fz, 128));
      while(a < z && *a != '\n') ++a;
      if(a < z) ++a;
    } else {
      while(a < base + fz && *(a - 1) != '\n') ++a;
    }
    if(k + 1 < NC){
      while(z < base + fz && *(z - 1) != '\n') ++z;
    }

    P[k].reserve((z - a) / 12 + 16);
    while(a < z){
      while(a < z && (unsigned)(*a - '0') > 9) ++a;
      if(a >= z) break;
      u32 u = num(a, z), v = num(a, z), w = num(a, z);
      if(u != v) P[k].push_back({u, v, w});
    }
  });

  // ---- CSR Build ----
  const int SH = 15; size_t B = (n >> SH) + 1, E = 0, bk = (n + NC - 1) / NC;
  auto each = [&](int d, size_t k, auto f){
    if(!d){ for(auto& e : P[k]) f(e); }
    else F(u, k * bk, min(n, (k + 1) * bk)) F(i, off[0][u], off[0][u+1]) f(E3{{(u32)u, adj[0][i].to, adj[0][i].w}});
  };

  vector<size_t> C(NC * B), bs(B + 1);
  F(d, 0, 2){
    fill(C.begin(), C.end(), 0);
    dyn(NC, [&](size_t k){ each(d, k, [&](const E3& e){ ++C[k * B + (e.a[d] >> SH)]; }); });
    size_t r = 0; F(b, 0, B){ bs[b] = r; F(k, 0, NC){ size_t c = C[k * B + b]; C[k * B + b] = r; r += c; } } bs[B] = E = r;
    E3* tmp = new E3[E + 1];
    dyn(NC, [&](size_t k){ each(d, k, [&](const E3& e){ tmp[C[k * B + (e.a[d] >> SH)]++] = e; }); });
    if(!d){ P.clear(); P.shrink_to_fit(); }
    off[d] = (u32*)malloc((n + 1) * 4); adj[d] = (Edge*)malloc((E + 1) * 8); off[d][n] = E;
    dyn(B, [&](size_t b){
      size_t lo = b << SH, hi = min(n, (b + 1) << SH), i0 = bs[b], i1 = bs[b+1];
      vector<u32> c(hi > lo ? hi - lo : 0);
      F(i, i0, i1) ++c[tmp[i].a[d] - lo];
      u32 cur_r = i0; F(j, 0, c.size()){ u32 x = c[j]; off[d][lo + j] = cur_r; c[j] = cur_r; cur_r += x; }
      F(i, i0, i1){ auto& e = tmp[i]; adj[d][c[e.a[d] - lo]++] = {e.a[1-d], e.a[2]}; }
    });
    delete[] tmp;
  }
  munmap((void*)base, fz);

  // ---- Renumber nodes by BFS locality if scattered ----
  vector<u32> nid;
  {
    u64 nr = 0, tot = 0; size_t sd = max<size_t>(1, n / 200000);
    for(size_t u = 0; u < n; u += sd) for(u32 i = off[0][u]; i < off[0][u+1]; ++i){
      size_t v = adj[0][i].to; ++tot; nr += (u > v ? u - v : v - u) <= 4096;
    }
    if(tot && nr * 10 < tot * 6 && n > 1){
      vector<char> vis(n, 0);
      auto claim = [&](u32 v){ return !vis[v] && !__atomic_exchange_n(&vis[v], 1, __ATOMIC_RELAXED); };
      auto grow = [&](vector<u32>& q){
        for(size_t h = 0; h < q.size(); ++h){
          u32 u = q[h];
          if(h + 16 < q.size()) F(d, 0, 2) __builtin_prefetch(off[d] + q[h+16]);
          if(h + 8 < q.size()) F(d, 0, 2) __builtin_prefetch(adj[d] + off[d][q[h+8]]);
          F(d, 0, 2) for(u32 i = off[d][u]; i < off[d][u+1]; ++i) if(claim(adj[d][i].to)) q.push_back(adj[d][i].to);
        }
      };
      vector<vector<u32>> L(NT + 1);
      par(NT, [&](unsigned k){
        auto& q = L[k]; q.reserve(n / NT * 3 / 2 + 1024);
        u32 s = (u64)n * k / NT; if(claim(s)){ q.push_back(s); grow(q); }
      });
      F(r, 0, n) if(claim(r)){ L[NT].push_back(r); grow(L[NT]); }
      vector<u32> ord; ord.reserve(n);
      for(auto& q : L) ord.insert(ord.end(), q.begin(), q.end());
      size_t bt = (n + NT - 1) / NT; nid.resize(n);
      par(NT, [&](unsigned k){ F(i, k * bt, min(n, (k + 1) * bt)) nid[ord[i]] = i; });
      F(d, 0, 2){
        u32* no = (u32*)malloc((n + 1) * 4); Edge* na = (Edge*)malloc((E + 1) * 8); no[0] = 0;
        par(NT, [&](unsigned k){ F(i, k * bt, min(n, (k + 1) * bt)) no[i+1] = off[d][ord[i]+1] - off[d][ord[i]]; });
        F(i, 0, n) no[i+1] += no[i];
        par(NT, [&](unsigned k){
          F(i, k * bt, min(n, (k + 1) * bt)){ u32 w = no[i], o = ord[i]; F(j, off[d][o], off[d][o+1]) na[w++] = {nid[adj[d][j].to], adj[d][j].w}; }
        });
        free(off[d]); free(adj[d]); off[d] = no; adj[d] = na;
      }
    }
  }

  // ---- Parse Queries ----
  vector<pair<u32,u32>> Q;
  {
    int qfd = open("/data/queries.txt", O_RDONLY);
    if(qfd >= 0){
      struct stat qsb; fstat(qfd, &qsb);
      u64 qfz = qsb.st_size;
      const char* qbase = (char*)mmap(NULL, qfz, PROT_READ, MAP_SHARED, qfd, 0);
      const char* qp = qbase;
      const char* qe = qp + qfz;
      while(qp < qe){
        while(qp < qe && (unsigned)(*qp - '0') > 9) ++qp;
        if(qp >= qe) break;
        u32 s = num(qp, qe), t = num(qp, qe);
        if(!nid.empty()){ s = nid[s]; t = nid[t]; }
        Q.push_back({s, t});
      }
      munmap((void*)qbase, qfz);
      close(qfd);
    }
  }

  vector<long long> ans(Q.size());
  vector<u32> od(Q.size()); iota(od.begin(), od.end(), 0);
  sort(od.begin(), od.end(), [&](u32 a, u32 b){ return Q[a].first < Q[b].first; });
  vector<u32>().swap(nid);

  // ---- Query Processing ----
  size_t N = Q.size(); atomic<size_t> nx{0};
  par(min<size_t>(NT, max<size_t>(1, N)), [&](unsigned){
    Sc S;
    S.D[0] = (u64*)calloc(n, 8);
    S.D[1] = (u64*)calloc(n, 8);
    S.ep = 0;
    for(size_t i = nx; i < N; i = nx){
      size_t c = i < N * 2 / 3 ? (3 * N + 4 * NT - 1) / (4 * NT) : max<size_t>(2, (N - i) / (2 * NT));
      if(!nx.compare_exchange_weak(i, i + c)) continue;
      F(j, i, min(i + c, N)){
        u32 q = od[j];
        ans[q] = solve(S, Q[q].first, Q[q].second);
      }
    }
    free(S.D[0]); free(S.D[1]);
  });

  // ---- Output Distances ----
  string o; o.reserve(N * 20);
  for(auto a : ans){
    o += to_string(a);
    o.push_back('\n');
  }
  int out_fd = open("/out/distances.txt", O_WRONLY | O_CREAT | O_TRUNC, 0666);
  if(out_fd >= 0){
    for(size_t w = 0; w < o.size(); ){
      ssize_t written = write(out_fd, o.data() + w, o.size() - w);
      if(written <= 0) break;
      w += written;
    }
    close(out_fd);
  }
  _exit(0);
}