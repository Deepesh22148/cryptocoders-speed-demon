// Track A v2: parallel chunk parse -> per-hour paged record store (6 B/line) -> per-hour in-cache counting sort,
// selection and formatting, written out in hour order. No global arrays; the record pages are returned as they are consumed.
#include <bits/stdc++.h>
#include <emmintrin.h>
#include <fcntl.h>
#include <sched.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/stat.h>
#include <unistd.h>
using namespace std;
typedef uint32_t u32; typedef uint64_t u64; typedef int64_t i64; typedef uint16_t u16;

static i64 days_from_civil(i64 y, unsigned m, unsigned d) {
  y -= m <= 2;
  i64 era = (y >= 0 ? y : y - 399) / 400;
  unsigned yoe = (unsigned)(y - era * 400);
  unsigned doy = (153 * (m + (m > 2 ? -3 : 9)) + 2) / 5 + d - 1;
  unsigned doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
  return era * 146097 + (i64)doe - 719468;
}
static void civil_from_days(i64 z, i64& y, unsigned& m, unsigned& d) {
  z += 719468;
  i64 era = (z >= 0 ? z : z - 146096) / 146097;
  unsigned doe = (unsigned)(z - era * 146097);
  unsigned yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
  unsigned doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
  unsigned mp = (5 * doy + 2) / 153;
  d = doy - (153 * mp + 2) / 5 + 1;
  m = mp < 10 ? mp + 3 : mp - 9;
  y = yoe + era * 400 + (m <= 2);
}

constexpr u64 CH = 512 << 10;     // bytes per work chunk
constexpr size_t HRS = 72;        // the span is fixed: 72 hours from 2026-10-30T18:00Z
constexpr u32 BLK = 640, BSZ = 4096; // one 4 KiB page: 640 latencies (u32) then 640 metas (u16: endpoint id | is5xx << 15)

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

struct Slot { u64 h; u32 id; };
struct Tab {   // per-thread endpoint dictionary
  vector<Slot> s{4096}; vector<char> arena; vector<u32> off; vector<u16> len; u64 mask = 4095;
  void grow() {
    vector<Slot> n(s.size() * 2); u64 m = n.size() - 1;
    for (auto& x : s) if (x.h) { u64 i = (x.h >> 20) & m; while (n[i].h) i = (i + 1) & m; n[i] = x; }
    s.swap(n); mask = m;
  }
  inline u32 find(const char* p, u32 l) {
    u64 h = hash_ep(p, l);
    for (u64 i = (h >> 20) & mask;; i = (i + 1) & mask) {
      Slot& x = s[i];
      if (x.h == h) return x.id;
      if (!x.h) {
        u32 id = off.size(); off.push_back(arena.size()); len.push_back(l); arena.insert(arena.end(), p, p + l);
        x = {h, id}; if (off.size() * 2 > s.size()) grow(); return id;
      }
    }
  }
};
struct TS {    // per-thread parse state: current page and finished pages of every hour
  Tab tab; char* cur[HRS]; u32 fill[HRS]; vector<char*> full[HRS];
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
  prctl(PR_SET_THP_DISABLE, 1, 0, 0, 0);   // exact 4 KiB accounting, no 2 MiB surprises
  const char* data = "/data/requests.log"; const char* outp = "/out/summary.txt";
  for (int i = 1; i + 1 < argc; i++) { if (!strcmp(argv[i], "--data")) data = argv[++i]; else if (!strcmp(argv[i], "--out")) outp = argv[++i]; }
  int fd = open(data, O_RDONLY); struct stat sb; if (fd < 0 || fstat(fd, &sb)) return 1;
  u64 fz = sb.st_size;
  int ofd = open(outp, O_WRONLY | O_CREAT | O_TRUNC, 0644);
  if (!fz) return 0;

  unsigned T = 12; { cpu_set_t c; if (!sched_getaffinity(0, sizeof c, &c)) T = min<int>(T, max(1, CPU_COUNT(&c))); }
  auto par = [&](unsigned n, auto f) { vector<thread> v; for (unsigned k = 1; k < n; ++k) v.emplace_back(f, k); f(0); for (auto& t : v) t.join(); };
  auto dyn = [&](size_t N, auto f) { atomic<size_t> c{0}; par(T, [&](unsigned t) { size_t k; while ((k = c++) < N) f(t, k); }); };

  const size_t NC = (fz + CH - 1) / CH;
  const size_t maxBlocks = fz / 40 / BLK + HRS * T + 16;   // a line is >= 40 bytes
  char* pool = (char*)mmap(nullptr, maxBlocks * BSZ, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0);
  atomic<u32> pn{0};
  vector<TS> ts(T);
  const i64 baseEH = days_from_civil(2026, 10, 30) * 24 + 18;

  vector<vector<char>> bufs(T);
  // ---- 1. parse ----
  dyn(NC, [&](unsigned t, size_t k) {
    TS& st = ts[t];
    u64 r0 = k * CH, r1 = min(fz, r0 + CH), e = min(fz, r1 + 256);
    const char* base;
    {
      u64 s = k ? r0 - 1 : 0; size_t len = e - s;
      vector<char>& bf = bufs[t]; if (bf.size() < CH + 1 + 256 + 64) bf.resize(CH + 1 + 256 + 64);
      for (size_t o = 0; o < len;) { ssize_t r = pread(fd, bf.data() + o, len - o, s + o); if (r <= 0) break; o += r; }
      memset(bf.data() + len, 0, 64);
      base = bf.data();
    }
    const char* p = base; const char* lim = base + (r1 - (k ? r0 - 1 : 0));
    if (k) p = (const char*)memchr(p, '\n', 256 + CH) + 1;     // first line starting at or after r0
    u64 ck = ~0ull; u32 ch = 0;
    const __m128i sp = _mm_set1_epi8(' ');
    while (p < lim) {
      u64 key = rd(p + 5);                                       // "MM-DDTHH"
      if (key != ck) {
        unsigned mo = (p[5] - '0') * 10 + p[6] - '0', da = (p[8] - '0') * 10 + p[9] - '0', hh = (p[11] - '0') * 10 + p[12] - '0';
        i64 y = (p[0] - '0') * 1000 + (p[1] - '0') * 100 + (p[2] - '0') * 10 + p[3] - '0';
        i64 h = days_from_civil(y, mo, da) * 24 + hh - baseEH; ch = h < 0 ? 0 : h >= (i64)HRS ? HRS - 1 : (u32)h; ck = key;
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
      if (f == BLK) { if (st.cur[ch]) st.full[ch].push_back(st.cur[ch]); st.cur[ch] = pool + (size_t)pn.fetch_add(1, memory_order_relaxed) * BSZ; f = 0; }
      char* b = st.cur[ch]; ((u32*)b)[f] = lat; ((u16*)(b + BLK * 4))[f] = meta; st.fill[ch] = f + 1;
      p = q + 1;
    }
  });

  // ---- 2. global endpoint ranks (bytewise order) ----
  vector<string_view> names;
  for (auto& s : ts) for (size_t i = 0; i < s.tab.off.size(); ++i) names.emplace_back(s.tab.arena.data() + s.tab.off[i], s.tab.len[i]);
  sort(names.begin(), names.end()); names.erase(unique(names.begin(), names.end()), names.end());
  const size_t E = names.size();
  vector<vector<u32>> remap(T);
  for (unsigned t = 0; t < T; ++t) for (size_t i = 0; i < ts[t].tab.off.size(); ++i)
    remap[t].push_back(lower_bound(names.begin(), names.end(), string_view(ts[t].tab.arena.data() + ts[t].tab.off[i], ts[t].tab.len[i])) - names.begin());
  char hs[HRS][24];
  for (size_t h = 0; h < HRS; ++h) { i64 y; unsigned mo, da; i64 eh = baseEH + h; civil_from_days(eh / 24, y, mo, da); snprintf(hs[h], 24, "%04lld-%02u-%02uT%02u:00:00Z ", (long long)y, mo, da, (unsigned)(eh % 24)); }

  // ---- 3. one task per hour: counting sort by endpoint rank, percentiles, text; output released in hour order ----
  vector<string> outs(HRS); vector<char> ready(HRS, 0); size_t nextw = 0; mutex mu;
  dyn(HRS, [&](unsigned, size_t h) {
    thread_local vector<u32> cn, er, pos, lat;
    cn.assign(E + 1, 0); er.assign(E + 1, 0);
    auto each = [&](auto f) {       // f(thread, page, count)
      for (unsigned t = 0; t < T; ++t) {
        for (char* b : ts[t].full[h]) f(t, b, BLK);
        if (ts[t].cur[h]) f(t, ts[t].cur[h], ts[t].fill[h]);
      }
    };
    each([&](unsigned t, char* b, u32 n) {
      const u32* rm = remap[t].data(); const u16* mt = (const u16*)(b + BLK * 4);
      for (u32 i = 0; i < n; ++i) { u32 m = mt[i]; u32 r = rm[m & 0x7fff]; cn[r]++; er[r] += m >> 15; }
    });
    pos.assign(E + 1, 0); u32 tot = 0;
    for (size_t r = 0; r < E; ++r) { pos[r] = tot; tot += cn[r]; }
    lat.resize(tot + 1);
    each([&](unsigned t, char* b, u32 n) {
      const u32* rm = remap[t].data(); const u32* lt = (const u32*)b; const u16* mt = (const u16*)(b + BLK * 4);
      for (u32 i = 0; i < n; ++i) lat[pos[rm[mt[i] & 0x7fff]]++] = lt[i];
      madvise(b, BSZ, MADV_DONTNEED);
    });
    string o; o.reserve(tot * 2 + 4096); char buf[512]; u32 start = 0;
    for (size_t r = 0; r < E; ++r) {
      u64 n = cn[r]; if (!n) continue;
      u32* v = lat.data() + start; start += n;
      auto ix = [&](u64 p) { return (p * n + 99) / 100 - 1; };
      u64 i50 = ix(50), i95 = ix(95), i99 = ix(99); u32 a, b, c;
      if (n <= 16) {
        for (u64 i = 1; i < n; ++i) { u32 x = v[i]; u64 j = i; while (j && v[j - 1] > x) { v[j] = v[j - 1]; --j; } v[j] = x; }
        a = v[i50]; b = v[i95]; c = v[i99];
      } else if (n < 48) {
        nth_element(v, v + i50, v + n); a = v[i50];
        if (i95 == i50) b = a; else { nth_element(v + i50 + 1, v + i95, v + n); b = v[i95]; }
        if (i99 == i95) c = b; else { nth_element(v + i95 + 1, v + i99, v + n); c = v[i99]; }
      } else { u64 rr[3] = {i50, i95, i99}; u32 o3[3]; select3(v, n, rr, o3); a = o3[0]; b = o3[1]; c = o3[2]; }
      u64 er_ = er[r]; char* w = buf;
      memcpy(w, hs[h], 21); w += 21; memcpy(w, names[r].data(), names[r].size()); w += names[r].size();
      *w++ = ' '; w = wu(w, n); *w++ = ' '; w = wu(w, a); *w++ = ' '; w = wu(w, b); *w++ = ' '; w = wu(w, c);
      *w++ = ' '; w = wu(w, er_); *w++ = ' '; w = wu(w, er_ * 1000000ull / n); *w++ = '\n';
      o.append(buf, w - buf);
    }
    lock_guard<mutex> g(mu);
    outs[h] = move(o); ready[h] = 1;
    while (nextw < HRS && ready[nextw]) {
      string& s = outs[nextw];
      for (size_t k = 0; k < s.size();) { ssize_t w = write(ofd, s.data() + k, s.size() - k); if (w <= 0) _exit(1); k += w; }
      string().swap(s); ++nextw;
    }
  });
  _exit(0);
}