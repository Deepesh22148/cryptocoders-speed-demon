// main.cpp -- g++ -O3 -std=c++23 -pthread -static -o solution main.cpp
#include <algorithm>
#include <atomic>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <map>
#include <string>
#include <string_view>
#include <thread>
#include <unordered_map>
#include <vector>
#include <fcntl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

static int64_t days_from_civil(int64_t y, unsigned m, unsigned d) {
    y -= m <= 2;
    int64_t era = (y >= 0 ? y : y - 399) / 400;
    unsigned yoe = (unsigned)(y - era * 400);
    unsigned doy = (153 * (m + (m > 2 ? -3 : 9)) + 2) / 5 + d - 1;
    unsigned doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    return era * 146097 + (int64_t)doe - 719468;
}
static void civil_from_days(int64_t z, int64_t& y, unsigned& m, unsigned& d) {
    z += 719468;
    int64_t era = (z >= 0 ? z : z - 146096) / 146097;
    unsigned doe = (unsigned)(z - era * 146097);
    unsigned yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    int64_t yy = yoe + era * 400;
    unsigned doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    unsigned mp = (5 * doy + 2) / 153;
    d = doy - (153 * mp + 2) / 5 + 1;
    m = mp < 10 ? mp + 3 : mp - 9;
    y = yy + (m <= 2);
}

struct Group { std::vector<uint32_t> v; uint64_t err = 0; };

struct Local {
    std::unordered_map<std::string_view, uint32_t> ep_id;
    std::vector<std::string_view> ep_names;
    std::unordered_map<uint64_t, Group> groups;   // (epochHour << 24) | epId
    uint64_t bad = 0, offsets = 0, big = 0;
};

static inline int dg(const char* p) { return *p - '0'; }

static void process(const char* b, const char* e, Local& L) {
    uint64_t last_key = ~0ull; Group* last_g = nullptr;
    char cprefix[13] = {0}; int64_t ceh = 0; bool cvalid = false;

    while (b < e) {
        const char* nl = (const char*)memchr(b, '\n', e - b);
        const char* le = nl ? nl : e;
        const char* next = nl ? nl + 1 : e;
        if (le > b && le[-1] == '\r') --le;
        if (le == b) { b = next; continue; }

        const char* sp1 = (const char*)memchr(b, ' ', le - b);
        if (!sp1 || sp1 - b < 20) { L.bad++; b = next; continue; }
        const char* sp2 = (const char*)memchr(sp1 + 1, ' ', le - sp1 - 1);
        if (!sp2) { L.bad++; b = next; continue; }
        const char* ep = sp2 + 1;
        const char* sp3 = (const char*)memchr(ep, ' ', le - ep);
        if (!sp3) { L.bad++; b = next; continue; }
        const char* st = sp3 + 1;
        const char* sp4 = (const char*)memchr(st, ' ', le - st);
        if (!sp4) { L.bad++; b = next; continue; }

        // ---- hour as epoch hour (UTC) ----
        int64_t eh;
        if (sp1[-1] == 'Z') {
            if (cvalid && memcmp(b, cprefix, 13) == 0) eh = ceh;
            else {
                int64_t y = dg(b)*1000 + dg(b+1)*100 + dg(b+2)*10 + dg(b+3);
                unsigned mo = dg(b+5)*10 + dg(b+6), da = dg(b+8)*10 + dg(b+9);
                int hh = dg(b+11)*10 + dg(b+12);
                eh = days_from_civil(y, mo, da) * 24 + hh;
                memcpy(cprefix, b, 13); ceh = eh; cvalid = true;
            }
        } else {
            L.offsets++;
            int64_t y = dg(b)*1000 + dg(b+1)*100 + dg(b+2)*10 + dg(b+3);
            unsigned mo = dg(b+5)*10 + dg(b+6), da = dg(b+8)*10 + dg(b+9);
            int hh = dg(b+11)*10 + dg(b+12), mi = dg(b+14)*10 + dg(b+15);
            int64_t mins = days_from_civil(y, mo, da) * 1440 + hh * 60 + mi;
            size_t tl = sp1 - b;
            if (tl >= 6 && (sp1[-6] == '+' || sp1[-6] == '-') && sp1[-3] == ':') {
                int off = dg(sp1 - 5) * 600 + dg(sp1 - 4) * 60 + dg(sp1 - 2) * 10 + dg(sp1 - 1);
                mins -= (sp1[-6] == '+' ? off : -off);
            }
            eh = mins >= 0 ? mins / 60 : -((-mins + 59) / 60);
        }

        // ---- status / latency ----
        int status = 0;
        for (const char* p = st; p < sp4; ++p) status = status * 10 + (*p - '0');
        uint64_t lat = 0;
        for (const char* p = sp4 + 1; p < le && *p >= '0' && *p <= '9'; ++p) lat = lat * 10 + (*p - '0');
        if (lat > 0xFFFFFFFFull) { L.big++; lat = 0xFFFFFFFFull; }

        std::string_view name(ep, sp3 - ep);
        uint32_t id;
        auto it = L.ep_id.find(name);
        if (it == L.ep_id.end()) {
            id = (uint32_t)L.ep_names.size();
            L.ep_id.emplace(name, id); L.ep_names.push_back(name);
        } else id = it->second;

        uint64_t key = ((uint64_t)eh << 24) | id;
        if (key != last_key) { last_g = &L.groups[key]; last_key = key; }
        last_g->v.push_back((uint32_t)lat);
        last_g->err += (status >= 500 && status <= 599);
        b = next;
    }
}

int main(int argc, char** argv) {
    std::string data = "/data", out = "/out";
    for (int i = 1; i + 1 < argc; i++) {
        if (!strcmp(argv[i], "--data")) data = argv[++i];
        else if (!strcmp(argv[i], "--out")) out = argv[++i];
    }
    mkdir(out.c_str(), 0755);

    std::string path = data + "/requests.log";
    int fd = open(path.c_str(), O_RDONLY);
    if (fd < 0) { perror(path.c_str()); return 1; }
    struct stat sb;
    if (fstat(fd, &sb) != 0) { perror("fstat"); return 1; }

    std::string result;
    if (sb.st_size > 0) {
        size_t size = sb.st_size;
        const char* base = (const char*)mmap(nullptr, size, PROT_READ, MAP_PRIVATE | MAP_POPULATE, fd, 0);
        if (base == MAP_FAILED) { perror("mmap"); return 1; }
        const char* end = base + size;

        unsigned T = std::max(1u, std::thread::hardware_concurrency());
        if (size < (1u << 20)) T = 1;
        std::vector<const char*> cut(T + 1);
        cut[0] = base; cut[T] = end;
        for (unsigned i = 1; i < T; i++) {
            const char* q = std::max(base + size / T * i, cut[i - 1]);
            const char* nl = (const char*)memchr(q, '\n', end - q);
            cut[i] = nl ? nl + 1 : end;
        }

        std::vector<Local> locals(T);
        {
            std::vector<std::thread> th;
            for (unsigned i = 0; i < T; i++)
                th.emplace_back([&, i] { process(cut[i], cut[i + 1], locals[i]); });
            for (auto& t : th) t.join();
        }
        uint64_t bad = 0, offs = 0, big = 0;
        for (auto& L : locals) { bad += L.bad; offs += L.offsets; big += L.big; }
        if (bad || offs || big)
            fprintf(stderr, "WARN malformed=%llu non-Z-timestamps=%llu latency>u32=%llu\n",
                    (unsigned long long)bad, (unsigned long long)offs, (unsigned long long)big);

        std::map<std::pair<uint64_t, std::string>, std::vector<Group*>> merged;
        for (auto& L : locals)
            for (auto& [key, g] : L.groups)
                merged[{key >> 24, std::string(L.ep_names[key & 0xFFFFFF])}].push_back(&g);

        std::vector<std::pair<const std::pair<uint64_t, std::string>*, std::vector<Group*>*>> items;
        for (auto& kv : merged) items.push_back({&kv.first, &kv.second});
        std::vector<std::string> lines(items.size());

        std::atomic<size_t> next{0};
        auto worker = [&] {
            std::vector<uint32_t> all;
            for (size_t i; (i = next.fetch_add(1)) < items.size();) {
                auto& gs = *items[i].second;
                uint64_t errs = 0;
                std::vector<uint32_t>* vp;
                if (gs.size() == 1) vp = &gs[0]->v;
                else {
                    all.clear();
                    for (auto* g : gs) all.insert(all.end(), g->v.begin(), g->v.end());
                    vp = &all;
                }
                for (auto* g : gs) errs += g->err;
                auto& v = *vp;
                uint64_t n = v.size();
                auto idx = [&](uint64_t p) { return (p * n + 99) / 100 - 1; };
                uint64_t i50 = idx(50), i95 = idx(95), i99 = idx(99);
                std::nth_element(v.begin(), v.begin() + i99, v.end());
                uint32_t p99 = v[i99], p95, p50;
                if (i95 == i99) p95 = p99;
                else { std::nth_element(v.begin(), v.begin() + i95, v.begin() + i99); p95 = v[i95]; }
                if (i50 == i95) p50 = p95;
                else { std::nth_element(v.begin(), v.begin() + i50, v.begin() + i95); p50 = v[i50]; }

                uint64_t eh = items[i].first->first;
                int64_t y; unsigned mo, da;
                civil_from_days((int64_t)(eh / 24), y, mo, da);
                char buf[96];
                int len = snprintf(buf, sizeof buf, "%04lld-%02u-%02uT%02u:00:00Z ",
                                   (long long)y, mo, da, (unsigned)(eh % 24));
                std::string& o = lines[i];
                o.assign(buf, len);
                o += items[i].first->second;
                len = snprintf(buf, sizeof buf, " %llu %u %u %u %llu %llu\n",
                               (unsigned long long)n, p50, p95, p99,
                               (unsigned long long)errs, (unsigned long long)(errs * 1000000ull / n));
                o.append(buf, len);
            }
        };
        {
            std::vector<std::thread> th;
            for (unsigned i = 0; i < T; i++) th.emplace_back(worker);
            for (auto& t : th) t.join();
        }
        for (auto& l : lines) result += l;
    }

    FILE* f = fopen((out + "/summary.txt").c_str(), "wb");
    if (!f) { perror("summary.txt"); return 1; }
    fwrite(result.data(), 1, result.size(), f);
    fclose(f);
    return 0;
}