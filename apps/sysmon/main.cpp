// Sysmon — C++ Lumen demo.
//
// Talks to Lumen exclusively through the C ABI in <lumen.h> (wrapped
// by the header-only C++ API in <lumen.hpp>). No Rhai builtins —
// every signal the markup binds against is driven from this binary
// over the thread-safe signal channel. main.rhai stays small (initial
// seed) and is technically optional.

#include <lumen.hpp>

#include <algorithm>
#include <atomic>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <sstream>
#include <string>
#include <thread>
#include <unordered_map>
#include <vector>

namespace fs = std::filesystem;
using namespace std::chrono_literals;
using lumen::Value;

// Typed signal handles — one per bound label the markup reads. Assigning
// (`cpu_label = "…"`) pushes the value to the runtime; `bind-text` observes
// it next tick. Constructed at load time (naming a signal makes no FFI
// call), so they are safe as file-scope statics.
static lumen::Signal<std::string> cpu_label{"cpu_label"};
static lumen::Signal<std::string> mem_label{"mem_label"};
static lumen::Signal<std::string> mem_sub_label{"mem_sub_label"};
static lumen::Signal<std::string> cpu_cores_label{"cpu_cores_label"};
static lumen::Signal<std::string> cpu_histo{"cpu_histo_text"};
static lumen::Signal<std::string> mem_histo{"mem_histo_text"};
static lumen::Signal<std::string> updated_label{"updated_label"};
static lumen::Signal<std::string> proc_label{"proc_label"};

// ---------- /proc parsing ----------

struct CpuStat {
    uint64_t user = 0, nice = 0, sys = 0, idle = 0;
    uint64_t iowait = 0, irq = 0, softirq = 0, steal = 0;
    uint64_t total() const { return user + nice + sys + idle + iowait + irq + softirq + steal; }
    uint64_t busy()  const { return total() - idle - iowait; }
};

static bool parse_cpu_line(const std::string& line, CpuStat& out) {
    std::istringstream is(line);
    std::string tag;
    is >> tag >> out.user >> out.nice >> out.sys >> out.idle
       >> out.iowait >> out.irq >> out.softirq >> out.steal;
    return !is.fail();
}

static CpuStat read_cpu_total() {
    std::ifstream f("/proc/stat");
    std::string line;
    CpuStat s{};
    if (std::getline(f, line)) parse_cpu_line(line, s);
    return s;
}

static std::vector<CpuStat> read_cpu_per_core() {
    std::vector<CpuStat> v;
    std::ifstream f("/proc/stat");
    std::string line;
    while (std::getline(f, line)) {
        if (line.rfind("cpu", 0) != 0) break;
        // First "cpu " line is the aggregate; per-core lines look like "cpu0", "cpu1", ...
        if (line.size() < 4 || line[3] == ' ') continue;
        if (!std::isdigit(static_cast<unsigned char>(line[3]))) break;
        CpuStat s{};
        if (parse_cpu_line(line, s)) v.push_back(s);
    }
    return v;
}

static uint64_t read_kb_field(const char* key) {
    std::ifstream f("/proc/meminfo");
    std::string line;
    size_t klen = std::strlen(key);
    while (std::getline(f, line)) {
        if (line.size() > klen && line.compare(0, klen, key) == 0 && line[klen] == ':') {
            std::istringstream is(line.substr(klen + 1));
            uint64_t v = 0;
            is >> v;
            return v;
        }
    }
    return 0;
}

struct ProcEntry {
    int         pid = 0;
    std::string name;
    uint64_t    cpu_jiffies = 0;
    uint64_t    rss_kb = 0;
};

static std::vector<ProcEntry> read_processes() {
    std::vector<ProcEntry> out;
    std::error_code ec;
    for (auto const& dirent : fs::directory_iterator("/proc", ec)) {
        if (!dirent.is_directory(ec)) continue;
        auto nm = dirent.path().filename().string();
        if (nm.empty() || !std::all_of(nm.begin(), nm.end(),
                                       [](unsigned char c) { return std::isdigit(c); })) {
            continue;
        }
        std::ifstream sf(dirent.path() / "stat");
        if (!sf) continue;
        std::string line;
        if (!std::getline(sf, line)) continue;
        auto open  = line.find('(');
        auto close = line.rfind(')');
        if (open == std::string::npos || close == std::string::npos || close <= open) continue;
        ProcEntry p{};
        p.pid  = std::atoi(nm.c_str());
        p.name = line.substr(open + 1, close - open - 1);
        std::istringstream is(line.substr(close + 2));
        std::string state;
        is >> state;
        // After comm + state, /proc/[pid]/stat has 10 fields before utime (field 14).
        for (int i = 0; i < 10; ++i) {
            std::string tmp;
            is >> tmp;
        }
        uint64_t utime = 0, stime = 0;
        is >> utime >> stime;
        // skip 8 fields to get to rss (field 24)
        for (int i = 0; i < 8; ++i) {
            std::string tmp;
            is >> tmp;
        }
        uint64_t rss_pages = 0;
        is >> rss_pages;
        p.cpu_jiffies = utime + stime;
        p.rss_kb      = rss_pages * 4;  // 4 KiB pages on x86_64
        out.push_back(std::move(p));
    }
    return out;
}

// ---------- glyph ladder for histo bars ----------

static const char* glyph_for(double pct) {
    static const char* g[] = {"▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"};
    int idx = static_cast<int>(pct / 12.5);
    if (idx < 0) idx = 0;
    if (idx > 7) idx = 7;
    return g[idx];
}

// ---------- sampler ----------

static std::atomic<bool> g_stop{false};

static void sampler_thread() {
    CpuStat prev_total = read_cpu_total();
    auto    prev_per   = read_cpu_per_core();
    std::unordered_map<int, uint64_t> prev_proc_cpu;

    auto last_proc_refresh = std::chrono::steady_clock::now() - 1h;
    std::string cpu_histo_text, mem_histo_text;
    int seq = 0;

    while (!g_stop.load(std::memory_order_relaxed)) {
        std::this_thread::sleep_for(500ms);
        if (g_stop.load()) break;

        CpuStat   now_total = read_cpu_total();
        double cpu_pct = 0.0;
        uint64_t dt = now_total.total() - prev_total.total();
        uint64_t db = now_total.busy() - prev_total.busy();
        if (dt > 0) cpu_pct = 100.0 * static_cast<double>(db) / static_cast<double>(dt);
        prev_total = now_total;

        uint64_t mem_total_kb = read_kb_field("MemTotal");
        uint64_t mem_avail_kb = read_kb_field("MemAvailable");
        uint64_t mem_used_kb  = (mem_total_kb > mem_avail_kb) ? mem_total_kb - mem_avail_kb : 0;
        double   mem_used_mb  = mem_used_kb / 1024.0;
        double   mem_total_mb = mem_total_kb / 1024.0;
        double   mem_pct      = mem_total_kb > 0
                                  ? 100.0 * static_cast<double>(mem_used_kb)
                                          / static_cast<double>(mem_total_kb)
                                  : 0.0;

        char buf[96];
        std::snprintf(buf, sizeof(buf), "%.2f%%", cpu_pct);
        cpu_label = buf;

        std::snprintf(buf, sizeof(buf), "%.1f GB", mem_used_mb / 1024.0);
        mem_label = buf;

        std::snprintf(buf, sizeof(buf), "of %.1f GB · %.1f%%", mem_total_mb / 1024.0, mem_pct);
        mem_sub_label = buf;

        std::snprintf(buf, sizeof(buf), "across %zu cores", prev_per.size());
        cpu_cores_label = buf;

        // Rolling histogram strings. Each block-art glyph is 3 bytes
        // in UTF-8. 32 glyphs fits inside the metric panel at the
        // chosen font-size without escaping.
        cpu_histo_text += glyph_for(cpu_pct);
        mem_histo_text += glyph_for(mem_pct);
        while (cpu_histo_text.size() > 32 * 3) cpu_histo_text.erase(0, 3);
        while (mem_histo_text.size() > 32 * 3) mem_histo_text.erase(0, 3);
        cpu_histo = cpu_histo_text;
        mem_histo = mem_histo_text;

        ++seq;
        std::snprintf(buf, sizeof(buf), "sample #%d", seq);
        updated_label = buf;

        auto now = std::chrono::steady_clock::now();
        if (now - last_proc_refresh >= 3s) {
            last_proc_refresh = now;

            auto procs = read_processes();
            std::unordered_map<int, uint64_t> cur_proc_cpu;
            cur_proc_cpu.reserve(procs.size());
            for (auto const& p : procs) cur_proc_cpu[p.pid] = p.cpu_jiffies;

            struct Ranked {
                int         pid;
                std::string name;
                double      cpu_delta;
                double      mem_mb;
            };
            std::vector<Ranked> ranked;
            ranked.reserve(procs.size());
            for (auto const& p : procs) {
                auto it = prev_proc_cpu.find(p.pid);
                uint64_t prev = it == prev_proc_cpu.end() ? p.cpu_jiffies : it->second;
                ranked.push_back({
                    p.pid, p.name,
                    static_cast<double>(p.cpu_jiffies - prev),
                    p.rss_kb / 1024.0,
                });
            }
            prev_proc_cpu = std::move(cur_proc_cpu);
            std::sort(ranked.begin(), ranked.end(),
                      [](auto const& a, auto const& b) { return a.cpu_delta > b.cpu_delta; });
            if (ranked.size() > 200) ranked.resize(200);

            std::vector<Value> rows;
            rows.reserve(ranked.size());
            for (auto const& r : ranked) {
                char cpu_str[32], mem_str[32];
                std::snprintf(cpu_str, sizeof(cpu_str), "%.1f%%", r.cpu_delta);
                std::snprintf(mem_str, sizeof(mem_str), "%.0f MB", r.mem_mb);
                rows.push_back(Value::map({
                    {"id",      Value::string("p-" + std::to_string(r.pid))},
                    {"pid",     Value::string(std::to_string(r.pid))},
                    {"name",    Value::string(r.name)},
                    {"cpu_str", Value::string(cpu_str)},
                    {"mem_str", Value::string(mem_str)},
                }));
            }
            // Array signals aren't scalars, so they stay on the raw layer.
            lumen::raw::set_array("procs", Value::array(std::move(rows)));
            proc_label = std::to_string(ranked.size()) + " shown";
        }
    }
}

// ---------- main ----------

int main(int argc, char** argv) {
    const char* dir = argc > 1 ? argv[1] : LUMEN_APP_DIR;

    // Seed labels so the first paint isn't a wall of em-dashes. Assigning a
    // typed Signal handle pushes the value straight to the runtime.
    cpu_label = "…";
    mem_label = "…";
    mem_sub_label = "…";
    cpu_cores_label = "…";
    proc_label = "…";
    updated_label = "starting";
    cpu_histo = "";
    mem_histo = "";

    std::thread sampler(sampler_thread);

    // App verifies ABI compatibility and throws on a bad app dir; the
    // window title / size come from lumen.toml. run_checked() blocks until
    // the window closes and throws on a non-OK exit status.
    int rc = 0;
    try {
        lumen::App app(dir);
        app.run_checked();
    } catch (lumen::Error const& e) {
        std::fprintf(stderr, "sysmon: %s (status %d)\n", e.what(), e.status());
        rc = 1;
    }

    g_stop.store(true);
    if (sampler.joinable()) sampler.join();
    return rc;
}
