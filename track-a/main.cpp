// Track A: parallel pread-chunk parse -> compact records -> counting-sort by (hour, endpoint rank) -> per-group selection.
#include <bits/stdc++.h>
#include <emmintrin.h>
#include <fcntl.h>
#include <sched.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>
using namespace std;
typedef uint32_t u32; typedef uint64_t u64; typedef int64_t i64;

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

constexpr u64 CH = 512 << 10;   // bytes per work chunk (stays in L2)
constexpr size_t HRS = 72;      // the span is fixed: 72 hours from 2026-10-30T18:00Z
struct Rec { u32 lat, meta; };  // meta = local endpoint id | hour << 16 | is5xx << 24
struct Slot { u64 h; u32 id; };

// Per-thread endpoint dictionary (open addressing, names copied out of the read buffer).
struct Tab {
  vector<Slot> s{4096}; vector<char> arena; vector<u32> off; vector<uint16_t> len; u64 mask = 4095;
  void grow() {
    vector<Slot> n(s.size() * 2); u64 m = n.size() - 1;
    for (auto& x : s) if (x.id) { u64 i = (x.h >> 7) & m; while (n[i].id) i = (i + 1) & m; n[i] = x; }
    s.swap(n); mask = m;
  }
  u32 find(const char* p, u32 l, u64 h) {
    for (u64 i = (h >> 7) & mask;; i = (i + 1) & mask) {
      Slot& x = s[i];
      if (!x.id) {
        u32 id = off.size(); off.push_back(arena.size()); len.push_back(l); arena.insert(arena.end(), p, p + l);
        x = {h, id + 1}; if (off.size() * 2 > s.size()) grow(); return id;
      }
      if (x.h == h) { u32 id = x.id - 1; if (len[id] == l && !memcmp(&arena[off[id]], p, l)) return id; }
    }
  }
};

static inline u64 hash_bytes(const char* p, size_t n) {
  u64 h = n * 0x9E3779B97F4A7C15ull;
  while (n >= 8) { u64 w; memcpy(&w, p, 8); h = (h ^ w) * 0xff51afd7ed558ccdull; h ^= h >> 32; p += 8; n -= 8; }
  if (n) { u64 w; memcpy(&w, p, 8); w &= (1ull << (n * 8)) - 1; h = (h ^ w) * 0xc4ceb9fe1a85ec53ull; h ^= h >> 29; }
  h *= 0x9E3779B97F4A7C15ull; return h ^ (h >> 29);
}
static inline char* wu(char* o, u64 x) { char t[20]; int n = 0; do { t[n++] = '0' + x % 10; x /= 10; } while (x); while (n) *o++ = t[--n]; return o; }

int main(int argc, char** argv) {
  const char* data = "/data/requests.log"; const char* outp = "/out/summary.txt";
  for (int i = 1; i + 1 < argc; i++) { if (!strcmp(argv[i], "--data")) data = argv[++i]; else if (!strcmp(argv[i], "--out")) outp = argv[++i]; }
  int fd = open(data, O_RDONLY); struct stat sb; if (fd < 0 || fstat(fd, &sb)) return 1;
  u64 fz = sb.st_size;
  int ofd = open(outp, O_WRONLY | O_CREAT | O_TRUNC, 0644);
  if (!fz) return 0;

  unsigned T = 12; { cpu_set_t c; if (!sched_getaffinity(0, sizeof c, &c)) T = min<int>(T, max(1, CPU_COUNT(&c))); }
  auto par = [&](unsigned n, auto f) { vector<thread> v; for (unsigned k = 1; k < n; ++k) v.emplace_back(f, k); f(0); for (auto& t : v) t.join(); };
  auto dyn = [&](size_t N, auto f) { atomic<size_t> c{0}; par(T, [&](unsigned t) { size_t k; while ((k = c++) < N) f(t, k); }); };

  const size_t NC = (fz + CH - 1) / CH, capK = CH / 40 + 8;   // a line is >= 40 bytes
  Rec* R = (Rec*)mmap(nullptr, NC * capK * sizeof(Rec), PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0);
  vector<u32> cnt(NC); vector<Tab> tabs(T); vector<vector<u32>> mine(T);
  const i64 baseEH = days_from_civil(2026, 10, 30) * 24 + 18;

  // ---- 1. parse ----
  {
    vector<vector<char>> bufs(T);
    dyn(NC, [&](unsigned t, size_t k) {
      auto& buf = bufs[t]; if (buf.empty()) buf.resize(CH + 1 + 256 + 64);
      u64 r0 = k * CH, r1 = min(fz, r0 + CH), s = k ? r0 - 1 : 0, e = min(fz, r1 + 256);
      size_t len = e - s;
      for (size_t o = 0; o < len;) { ssize_t r = pread(fd, buf.data() + o, len - o, s + o); if (r <= 0) break; o += r; }
      memset(buf.data() + len, 0, 64);
      const char* p = buf.data(); const char* lim = p + (r1 - s);
      if (k) p = (const char*)memchr(p, '\n', len) + 1;
      Tab& tb = tabs[t]; Rec* o = R + k * capK; u32 c = 0;
      u64 ck = ~0ull; u32 ch = 0;
      const __m128i sp = _mm_set1_epi8(' ');
      while (p < lim) {
        u64 key; memcpy(&key, p + 5, 8);                       // "MM-DDTHH"
        if (key != ck) {
          unsigned mo = (p[5] - '0') * 10 + p[6] - '0', da = (p[8] - '0') * 10 + p[9] - '0', hh = (p[11] - '0') * 10 + p[12] - '0';
          i64 y = (p[0] - '0') * 1000 + (p[1] - '0') * 100 + (p[2] - '0') * 10 + p[3] - '0';
          ch = (u32)((days_from_civil(y, mo, da) * 24 + hh - baseEH) & 0xff); ck = key;
        }
        const char* m = p + 28; while (*m != ' ') ++m;          // method
        const char* ep = m + 1; const char* q = ep;
        for (;;) { u32 mk = _mm_movemask_epi8(_mm_cmpeq_epi8(_mm_loadu_si128((const __m128i*)q), sp)); if (mk) { q += __builtin_ctz(mk); break; } q += 16; }
        u32 l = q - ep; u32 id = tb.find(ep, l, hash_bytes(ep, l));
        u32 err = q[1] == '5'; q += 5;
        u64 x = 0; for (unsigned d; (d = (unsigned char)*q - '0') < 10; ++q) x = x * 10 + d;
        o[c++] = {(u32)x, id | ch << 16 | err << 24};
        p = q + 1;
      }
      cnt[k] = c; mine[t].push_back(k);
    });
  }

  // ---- 2. global endpoint ranks (bytewise order) ----
  vector<string_view> names;
  for (auto& tb : tabs) for (size_t i = 0; i < tb.off.size(); ++i) names.emplace_back(tb.arena.data() + tb.off[i], tb.len[i]);
  sort(names.begin(), names.end()); names.erase(unique(names.begin(), names.end()), names.end());
  const u64 E = names.size(), G = HRS * E;
  vector<vector<u32>> remap(T);
  for (unsigned t = 0; t < T; ++t) for (size_t i = 0; i < tabs[t].off.size(); ++i)
    remap[t].push_back(lower_bound(names.begin(), names.end(), string_view(tabs[t].arena.data() + tabs[t].off[i], tabs[t].len[i])) - names.begin());

  // ---- 3. counting sort by group ----
  vector<u32*> hist(T); vector<atomic<u32>> err(G); vector<u32> tot(G), gs(G + 1);
  par(T, [&](unsigned t) {
    u32* h = hist[t] = (u32*)calloc(G, 4); const u32* rm = remap[t].data();
    for (u32 k : mine[t]) { const Rec* r = R + k * capK;
      for (u32 i = 0; i < cnt[k]; ++i) { u32 m = r[i].meta, g = ((m >> 16) & 0xff) * E + rm[m & 0xffff]; h[g]++; if (m >> 24) err[g].fetch_add(1, memory_order_relaxed); } }
  });
  par(T, [&](unsigned r) {
    for (u64 g = G * r / T; g < G * (r + 1) / T; ++g) { u32 s = 0; for (unsigned t = 0; t < T; ++t) { u32 c = hist[t][g]; hist[t][g] = s; s += c; } tot[g] = s; }
  });
  for (u64 g = 0; g < G; ++g) gs[g + 1] = gs[g] + tot[g];
  u32* LAT = (u32*)malloc((size_t)gs[G] * 4 + 16);
  par(T, [&](unsigned t) {
    u32* h = hist[t]; const u32* rm = remap[t].data();
    for (u32 k : mine[t]) { const Rec* r = R + k * capK;
      for (u32 i = 0; i < cnt[k]; ++i) { u32 m = r[i].meta, g = ((m >> 16) & 0xff) * E + rm[m & 0xffff]; LAT[gs[g] + h[g]++] = r[i].lat; } }
  });
  munmap(R, NC * capK * sizeof(Rec)); for (auto h : hist) free(h);

  // ---- 4. per-group percentiles and formatting ----
  char hs[HRS][24];
  for (size_t h = 0; h < HRS; ++h) { i64 y; unsigned mo, da; i64 eh = baseEH + h; civil_from_days(eh / 24, y, mo, da); snprintf(hs[h], 24, "%04lld-%02u-%02uT%02u:00:00Z ", (long long)y, mo, da, (unsigned)(eh % 24)); }
  const u64 RS = 512, NR = (G + RS - 1) / RS; vector<string> outs(NR);
  dyn(NR, [&](unsigned, size_t r) {
    string& o = outs[r]; o.reserve(RS * 70); char b[512];
    for (u64 g = r * RS; g < min(G, (r + 1) * RS); ++g) {
      u64 n = tot[g]; if (!n) continue;
      u32* v = LAT + gs[g];
      auto ix = [&](u64 p) { return (p * n + 99) / 100 - 1; };
      u64 i50 = ix(50), i95 = ix(95), i99 = ix(99); u32 a, bb, c;
      if (n <= 64) { sort(v, v + n); a = v[i50]; bb = v[i95]; c = v[i99]; }
      else {
        nth_element(v, v + i99, v + n); c = v[i99];
        if (i95 == i99) bb = c; else { nth_element(v, v + i95, v + i99); bb = v[i95]; }
        if (i50 == i95) a = bb; else { nth_element(v, v + i50, v + i95); a = v[i50]; }
      }
      u64 hr = g / E, ep = g % E; u64 er = err[g].load(memory_order_relaxed);
      char* w = b; memcpy(w, hs[hr], 21); w += 21;
      memcpy(w, names[ep].data(), names[ep].size()); w += names[ep].size();
      *w++ = ' '; w = wu(w, n); *w++ = ' '; w = wu(w, a); *w++ = ' '; w = wu(w, bb); *w++ = ' '; w = wu(w, c);
      *w++ = ' '; w = wu(w, er); *w++ = ' '; w = wu(w, er * 1000000ull / n); *w++ = '\n';
      o.append(b, w - b);
    }
  });
  for (auto& o : outs) for (size_t k = 0; k < o.size();) { ssize_t w = write(ofd, o.data() + k, o.size() - k); if (w <= 0) return 1; k += w; }
  _exit(0);
}