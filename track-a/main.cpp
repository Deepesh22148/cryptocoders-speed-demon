// Track A v4: the file is in arrival order, so an hour is complete once lines two hours later have been seen.
// Threads parse chunks in file order and close finished hours on the spot (sort by endpoint, percentiles, text),
// recycling the record pages. If an hour ever receives a line after it was closed, the run restarts in safe mode.
#include <bits/stdc++.h>
#include <emmintrin.h>
#include <fcntl.h>
#include <sched.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>
using namespace std;
typedef uint32_t u32; typedef uint64_t u64; typedef uint16_t u16; typedef uint8_t u8;

constexpr u64 CH = 128 << 10;                 // bytes per work chunk
constexpr size_t HRS = 72, MAXE = 4096;       // the span is 72 hours from 2026-10-30T18:00Z; 1842 endpoints
constexpr u32 BLK = 640, BSZ = 4096;          // one page: 640 latencies (u32) then 640 metas (u16: endpoint | is5xx << 15)

static inline u64 rd(const char* p) { u64 w; memcpy(&w, p, 8); return w; }
static inline u64 mum(u64 a, u64 b) { __uint128_t r = (__uint128_t)a * b; return (u64)r ^ (u64)(r >> 64); }
// 64-bit hash of the whole endpoint; equal hash is treated as equal name (collision odds ~1e-13 for ~1.8k names).
static inline u64 hash_ep(const char* p, u32 n) {
  const u64 S0 = 0xa0761d6478bd642full, S1 = 0xe7037ed1a0b428dbull;
  u64 a, b, seed = S0 ^ n;
  if (n >= 16) {
    const char* e = p + n;
    while (e - p > 16) { seed = mum(rd(p) ^ S1, rd(p + 8) ^ seed); p += 16; }
    a = rd(e - 16); b = rd(e - 8);
  } else if (n >= 8) { a = rd(p); b = rd(p + n - 8); }
  else { a = rd(p) & ((1ull << (n * 8)) - 1); b = 0; }
  return mum(a ^ S1, b ^ seed) | 1;
}

// Global endpoint registry: hash -> global id, plus the names in bytewise order (extended as endpoints appear).
static mutex rm; static unordered_map<u64, u32> byHash; static char nb[MAXE][128]; static u8 nl[MAXE];
static u32 nameCount, sortedCount, rankOf[MAXE]; static vector<u32> order;
static u32 reg(u64 h, const char* p, u32 l) {
  lock_guard<mutex> g(rm); auto it = byHash.try_emplace(h, nameCount);
  if (it.second) { memcpy(nb[nameCount], p, l); nl[nameCount++] = l; }
  return it.first->second;
}

struct Slot { u64 h; u32 id; };
struct Tab {   // per-thread endpoint dictionary: name -> local id, local id -> global id
  vector<Slot> s{4096}; u32 gid[MAXE]; atomic<u32> n{0};
  inline u32 find(const char* p, u32 l) {
    u64 h = hash_ep(p, l);
    for (u64 i = (h >> 20) & 4095;; i = (i + 1) & 4095) {
      Slot& x = s[i];
      if (x.h == h) return x.id;
      if (!x.h) { u32 id = n; gid[id] = reg(h, p, l); x = {h, id}; n.store(id + 1, memory_order_release); return id; }
    }
  }
};
struct TS {    // per-thread parse state: the current page and the finished pages of every hour
  Tab tab; char* cur[HRS]; u32 fill[HRS]; vector<char*> full[HRS]; u64 lines = 0;
  TS() { for (size_t h = 0; h < HRS; ++h) { cur[h] = nullptr; fill[h] = BLK; } }
};

static inline u64 parse_num(const char*& q) {   // decimal up to 10 digits, SWAR for <= 7
  u64 t = rd(q) ^ 0x3030303030303030ull;
  u64 nd = ((t + 0x7676767676767676ull) | t) & 0x8080808080808080ull;
  if (__builtin_expect(!nd, 0)) { u64 x = 0; for (unsigned d; (d = (unsigned char)*q - '0') < 10; ++q) x = x * 10 + d; return x; }
  unsigned n = __builtin_ctzll(nd) >> 3; q += n;
  t <<= (8 - n) * 8;
  t = (t * 10 + (t >> 8)) & 0x00FF00FF00FF00FFull;
  t = (t * 100 + (t >> 16)) & 0x0000FFFF0000FFFFull;
  return (t * 10000 + (t >> 32)) & 0xFFFFFFFFull;
}
static const char* D2 = "00010203040506070809101112131415161718192021222324252627282930313233343536373839404142434445464748495051525354555657585960616263646566676869707172737475767778798081828384858687888990919293949596979899";
static inline char* wu(char* o, u64 x) {
  char t[20]; char* e = t + 20; char* p = e;
  while (x >= 100) { u64 q = x / 100; memcpy(p -= 2, D2 + 2 * (x - q * 100), 2); x = q; }
  if (x >= 10) memcpy(p -= 2, D2 + 2 * x, 2); else *--p = '0' + x;
  size_t n = e - p; memcpy(o, p, n); return o + n;
}

// Exact p50/p95/p99 of a large group without sorting it: histogram on a log-scale key (16 sub-buckets per octave),
// then nth_element only inside the (at most three) buckets that hold the wanted ranks.
static inline u32 bkey(u32 v) { if (v < 16) return v; unsigned b = 31 - __builtin_clz(v); return ((b - 3) << 4) | ((v >> (b - 4)) & 15); }
static void select3(u32* v, u64 n, const u64 r[3], u32 out[3]) {
  thread_local vector<u32> tmp[3]; u32 cnt[464] = {0};
  for (u64 i = 0; i < n; ++i) cnt[bkey(v[i])]++;
  u32 kk[3], off[3]; int id[3]; int nid = 0; u64 cum = 0; size_t k = 0;
  for (int j = 0; j < 3; ++j) {
    while (cum + cnt[k] <= r[j]) cum += cnt[k++];
    kk[j] = k; off[j] = r[j] - cum;
    if (j && kk[j] == kk[j - 1]) id[j] = id[j - 1]; else { id[j] = nid++; tmp[id[j]].clear(); }
  }
  for (u64 i = 0; i < n; ++i) { u32 x = v[i], q = bkey(x); for (int j = 0; j < 3; ++j) if (q == kk[j]) { if (j == 0 || id[j] != id[j - 1]) tmp[id[j]].push_back(x); break; } }
  for (int j = 0; j < 3; ++j) { auto& a = tmp[id[j]]; nth_element(a.begin(), a.begin() + off[j], a.end()); out[j] = a[off[j]]; }
}

int main(int argc, char** argv) {
  const bool safe = argc > 1;   // safe mode: no hour is closed before the whole file has been read
  int fd = open("/data/requests.log", O_RDONLY); struct stat sb; if (fd < 0 || fstat(fd, &sb)) return 1;
  u64 fz = sb.st_size;
  int ofd = open("/out/summary.txt", O_WRONLY | O_CREAT | O_TRUNC, 0644);
  if (!fz) return 0;
  unsigned T = 12; { cpu_set_t c; if (!sched_getaffinity(0, sizeof c, &c)) T = min<int>(T, max(1, CPU_COUNT(&c))); }

  const size_t NC = (fz + CH - 1) / CH, maxBlocks = fz / 40 / BLK + HRS * T + 16;   // a line is >= 40 bytes
  char* pool = (char*)mmap(nullptr, maxBlocks * BSZ, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0);
  size_t poolNext = 0; vector<char*> freePages; mutex fm;
  auto newPage = [&]() -> char* {
    lock_guard<mutex> g(fm);
    if (freePages.empty()) return pool + poolNext++ * BSZ;
    char* p = freePages.back(); freePages.pop_back(); return p;
  };
  byHash.reserve(8192); vector<TS> ts(T); vector<vector<char>> bufs(T);

  // chunk bookkeeping: hours <= (highest hour in the finished prefix of chunks) - 2 are closed
  vector<u8> fin(NC, 0), mh(NC, 0); size_t pfx = 0; int front = 0; mutex pm;
  atomic<int> closable{0}, nextClose{0}; atomic<size_t> nextChunk{0}; atomic<u64> consumed{0};
  vector<string> outs(HRS); vector<char> ready(HRS, 0); size_t nextw = 0; mutex om;

  auto parse = [&](unsigned t, size_t k) {
    TS& st = ts[t];
    u64 r0 = k * CH, r1 = min(fz, r0 + CH), e = min(fz, r1 + 256), s = k ? r0 - 1 : 0; size_t len = e - s;
    vector<char>& bf = bufs[t]; if (bf.size() < CH + 1 + 256 + 64) bf.resize(CH + 1 + 256 + 64);
    for (size_t o = 0; o < len;) { ssize_t r = pread(fd, bf.data() + o, len - o, s + o); if (r <= 0) break; o += r; }
    memset(bf.data() + len, 0, 64);
    const char* p = bf.data(); const char* lim = p + (r1 - s);
    if (k) p = (const char*)memchr(p, '\n', 256 + CH) + 1;     // first line starting at or after r0
    u64 ck = ~0ull, cntl = 0; u32 ch = 0, mx = 0;
    const __m128i sp = _mm_set1_epi8(' ');
    while (p < lim) {
      u64 key = rd(p + 5);                                       // "MM-DDTHH"
      if (key != ck) {
        unsigned day = p[6] == '0' ? p[9] - '0' : p[9] - '1' + 2, hh = (p[11] - '0') * 10 + p[12] - '0';
        ch = min<unsigned>(day * 24 + hh - 18, HRS - 1); mx = max(mx, ch); ck = key;
      }
      const char* m = p + 28;                                    // method: 3..7 bytes then a space
      u64 y8 = rd(m) ^ 0x2020202020202020ull;
      const char* ep = m + (__builtin_ctzll((y8 - 0x0101010101010101ull) & ~y8 & 0x8080808080808080ull) >> 3) + 1;
      const char* q = ep;
      for (;;) { u32 mk = _mm_movemask_epi8(_mm_cmpeq_epi8(_mm_loadu_si128((const __m128i*)q), sp)); if (mk) { q += __builtin_ctz(mk); break; } q += 16; }
      u32 id = st.tab.find(ep, q - ep);
      u32 meta = id | (u32)(q[1] == '5') << 15;
      q += 5; u32 lat = (u32)parse_num(q);
      u32 f = st.fill[ch];
      if (f == BLK) { if (st.cur[ch]) st.full[ch].push_back(st.cur[ch]); st.cur[ch] = newPage(); f = 0; }
      char* b = st.cur[ch]; ((u32*)b)[f] = lat; ((u16*)(b + BLK * 4))[f] = meta; st.fill[ch] = f + 1;
      p = q + 1; ++cntl;
    }
    st.lines += cntl;
    lock_guard<mutex> g(pm); mh[k] = mx; fin[k] = 1;
    while (pfx < NC && fin[pfx]) front = max<int>(front, mh[pfx++]);
    closable.store(pfx == NC ? HRS : safe ? 0 : max(0, front - 1), memory_order_release);
  };

  const char* date[4] = {"2026-10-30", "2026-10-31", "2026-11-01", "2026-11-02"};
  auto hour = [&](size_t h) {
    thread_local vector<u32> cn, er, pos, lat; thread_local u32 rk[MAXE]; thread_local u16 rl[MAXE];
    u32 E;
    {
      lock_guard<mutex> g(rm);
      if (sortedCount != nameCount) {                           // bring the sorted endpoint list up to date
        size_t o = order.size();
        for (u32 x = sortedCount; x < nameCount; ++x) order.push_back(x);
        auto cmp = [](u32 a, u32 b) { return string_view(nb[a], nl[a]) < string_view(nb[b], nl[b]); };
        sort(order.begin() + o, order.end(), cmp); inplace_merge(order.begin(), order.begin() + o, order.end(), cmp);
        for (u32 r = 0; r < order.size(); ++r) rankOf[order[r]] = r;
        sortedCount = nameCount;
      }
      memcpy(rk, rankOf, sizeof rk); E = sortedCount;
    }
    cn.assign(E + 1, 0); er.assign(E + 1, 0);
    auto each = [&](auto f) {       // f(page, count, local id -> rank table of the owning thread)
      for (unsigned t = 0; t < T; ++t) {
        Tab& tb = ts[t].tab; u32 nn = tb.n.load(memory_order_acquire);
        for (u32 i = 0; i < nn; ++i) rl[i] = rk[tb.gid[i]];
        for (char* b : ts[t].full[h]) f(b, BLK);
        if (ts[t].cur[h]) f(ts[t].cur[h], ts[t].fill[h]);
      }
    };
    each([&](char* b, u32 n) {
      const u16* mt = (const u16*)(b + BLK * 4);
      for (u32 i = 0; i < n; ++i) { u32 m = mt[i]; u32 r = rl[m & 0x7fff]; cn[r]++; er[r] += m >> 15; }
    });
    pos.assign(E + 1, 0); u32 tot = 0;
    for (size_t r = 0; r < E; ++r) { pos[r] = tot; tot += cn[r]; }
    lat.resize(tot + 1);
    each([&](char* b, u32 n) {
      const u32* lt = (const u32*)b; const u16* mt = (const u16*)(b + BLK * 4);
      for (u32 i = 0; i < n; ++i) lat[pos[rl[mt[i] & 0x7fff]]++] = lt[i];
    });
    consumed += tot;
    {
      lock_guard<mutex> g(fm);      // recycle this hour's pages
      for (unsigned t = 0; t < T; ++t) {
        for (char* b : ts[t].full[h]) freePages.push_back(b);
        if (ts[t].cur[h]) freePages.push_back(ts[t].cur[h]);
        ts[t].full[h].clear(); ts[t].cur[h] = nullptr; ts[t].fill[h] = BLK;
      }
    }
    char hs[24]; snprintf(hs, 24, "%sT%02u:00:00Z ", date[(h + 18) / 24], (unsigned)(h + 18) % 24);
    string o; o.reserve(tot * 2 + 4096); char buf[512]; u32 start = 0;
    for (size_t r = 0; r < E; ++r) {
      u64 n = cn[r]; if (!n) continue;
      u32* v = lat.data() + start; start += n; u32 g = order[r];
      auto ix = [&](u64 p) { return (p * n + 99) / 100 - 1; };
      u64 i50 = ix(50), i95 = ix(95), i99 = ix(99); u32 a, b, c;
      if (n <= 16) {
        for (u64 i = 1; i < n; ++i) { u32 x = v[i]; u64 j = i; while (j && v[j - 1] > x) { v[j] = v[j - 1]; --j; } v[j] = x; }
        a = v[i50]; b = v[i95]; c = v[i99];
      } else { u64 rr[3] = {i50, i95, i99}; u32 o3[3]; select3(v, n, rr, o3); a = o3[0]; b = o3[1]; c = o3[2]; }
      u64 er_ = er[r]; char* w = buf;
      memcpy(w, hs, 21); w += 21; memcpy(w, nb[g], nl[g]); w += nl[g];
      *w++ = ' '; w = wu(w, n); *w++ = ' '; w = wu(w, a); *w++ = ' '; w = wu(w, b); *w++ = ' '; w = wu(w, c);
      *w++ = ' '; w = wu(w, er_); *w++ = ' '; w = wu(w, er_ * 1000000ull / n); *w++ = '\n';
      o.append(buf, w - buf);
    }
    lock_guard<mutex> g(om);        // write finished hours in order
    outs[h] = move(o); ready[h] = 1;
    while (nextw < HRS && ready[nextw]) {
      string& s = outs[nextw];
      for (size_t k = 0; k < s.size();) { ssize_t w = write(ofd, s.data() + k, s.size() - k); if (w <= 0) _exit(1); k += w; }
      string().swap(s); ++nextw;
    }
  };

  vector<thread> th;
  auto worker = [&](unsigned t) {
    for (;;) {
      int h = nextClose.load();
      while (h < closable.load(memory_order_acquire) && !nextClose.compare_exchange_weak(h, h + 1)) {}
      if (h < closable.load(memory_order_acquire)) { hour(h); continue; }
      size_t k = nextChunk.fetch_add(1);
      if (k < NC) { parse(t, k); continue; }
      if (nextClose.load() >= (int)HRS) break;
      this_thread::yield();      // the last chunks are still being parsed elsewhere
    }
  };
  for (unsigned t = 1; t < T; ++t) th.emplace_back(worker, t);
  worker(0);
  for (auto& x : th) x.join();
  u64 parsed = 0; for (auto& s : ts) parsed += s.lines;
  if (parsed != consumed) execl("/proc/self/exe", argv[0], "safe", (char*)0);   // an hour got a line after it was closed
  _exit(0);
}