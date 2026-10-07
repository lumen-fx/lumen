// Datagrid: proves Lumen handles a real-world live market feed.
// Pulls ~3000 spot tickers from Binance's public /api/v3/ticker/24hr
// every 5 s, virtualizes the rendered rows, keeps a per-symbol
// sparkline of the last 8 observed prices.

fn fmt_price(v) {
    let absv = v;
    if absv < 0.0 { absv = -absv; }
    let scale = 100;
    if absv < 1.0    { scale = 100000; }
    else if absv < 100.0 { scale = 1000; }
    let cents = (v * (scale.to_float())).to_int();
    let whole = cents / scale;
    let rem = cents % scale;
    if rem < 0 { rem = -rem; }
    let frac = "" + rem;
    let target = ("" + scale).len() - 1;
    while frac.len() < target { frac = "0" + frac; }
    "" + whole + "." + frac
}

fn fmt_volume(v) {
    if v >= 1000000000.0 {
        let m = (v / 100000000.0).to_int();
        let whole = m / 10;
        let r = m % 10;
        "" + whole + "." + r + "B"
    } else if v >= 1000000.0 {
        let m = (v / 100000.0).to_int();
        let whole = m / 10;
        let r = m % 10;
        "" + whole + "." + r + "M"
    } else if v >= 1000.0 {
        let m = (v / 100.0).to_int();
        let whole = m / 10;
        let r = m % 10;
        "" + whole + "." + r + "k"
    } else {
        "" + v.to_int()
    }
}

fn fmt_change(pct) {
    let sign = "";
    if pct > 0.0 { sign = "+"; }
    let cents = (pct * 100.0).to_int();
    let whole = cents / 100;
    let rem = cents % 100;
    if rem < 0 { rem = -rem; }
    let frac = "" + rem;
    if frac.len() < 2 { frac = "0" + frac; }
    sign + whole + "." + frac + "%"
}

// Bucket a normalized 0..1 trend value into one of 8 discrete height
// levels. The row markup picks a CSS class per bucket (lvl-0..lvl-7 in
// main.css) so the sparkline draws as real bars instead of a
// block-glyph string; that keeps the chart rendering independent of
// whatever font glyphs happen to be installed.
fn bucket_for(t) {
    if t < 0.125      { "0" }
    else if t < 0.25  { "1" }
    else if t < 0.375 { "2" }
    else if t < 0.5   { "3" }
    else if t < 0.625 { "4" }
    else if t < 0.75  { "5" }
    else if t < 0.875 { "6" }
    else              { "7" }
}

fn sparkline_buckets(hist) {
    let lo = hist[0];
    let hi = hist[0];
    let i = 1;
    while i < hist.len() {
        if hist[i] < lo { lo = hist[i]; }
        if hist[i] > hi { hi = hist[i]; }
        i += 1;
    }
    let range = hi - lo;
    if range < 0.0001 { range = 0.0001; }
    let out = [];
    let j = 0;
    while j < hist.len() {
        out.push(bucket_for((hist[j] - lo) / range));
        j += 1;
    }
    out
}

fn parse_f(s) { parse_float(s) }

fn dir_for(pct) {
    if pct > 0.0001  { "up" }
    else if pct < -0.0001 { "down" }
    else { "flat" }
}

fn empty_flag(n) {
    let v = "";
    if n == 0 { v = "1"; }
    v
}

fn refresh_counts(total, vis) {
    let label = "" + vis;
    if vis != total { label += " / " + total; }
    signal("rows_label", "").set(label);
}

fn roll_history(prev_hist, price) {
    let h2 = [];
    let k = 1;
    while k < prev_hist.len() {
        h2.push(prev_hist[k]);
        k += 1;
    }
    h2.push(price);
    h2
}

fn rebuild_visible() {
    let cur = signal_array("rows").all();
    let raw_filt = signal("filter", "").get();
    let filt = raw_filt.to_upper();
    let out = [];
    if filt == "" {
        out = cur;
    } else {
        let i = 0;
        while i < cur.len() {
            let r = cur[i];
            if r.symbol.contains(filt) { out.push(r); }
            i += 1;
        }
    }
    signal_array("visible").set(out);
    signal("empty_visible", "").set(empty_flag(out.len()));
    refresh_counts(cur.len(), out.len());
}

fn build_row(sym, price, pct, vol, hist) {
    let r = #{};
    r.id = sym;
    r.symbol = sym;
    r.price_f = price;
    r.history = hist;
    r.volume_f = vol;
    r.dir_class = dir_for(pct);
    r.price = fmt_price(price);
    r.volume = fmt_volume(vol);
    r.change = fmt_change(pct);
    let buckets = sparkline_buckets(hist);
    r.spark0 = buckets[0];
    r.spark1 = buckets[1];
    r.spark2 = buckets[2];
    r.spark3 = buckets[3];
    r.spark4 = buckets[4];
    r.spark5 = buckets[5];
    r.spark6 = buckets[6];
    r.spark7 = buckets[7];
    r
}

fn index_prev() {
    let prev = signal_array("rows").all();
    let prev_idx = #{};
    let i = 0;
    while i < prev.len() {
        let pr = prev[i];
        prev_idx[pr.symbol] = pr;
        i += 1;
    }
    prev_idx
}

fn coin_price(item) {
    let v = item.current_price;
    if type_of(v) == "f64" || type_of(v) == "float" { v }
    else if type_of(v) == "i64" || type_of(v) == "int" { v.to_float() }
    else { 0.0 }
}

fn coin_pct(item) {
    let v = item.price_change_percentage_24h;
    if type_of(v) == "f64" || type_of(v) == "float" { v }
    else if type_of(v) == "i64" || type_of(v) == "int" { v.to_float() }
    else { 0.0 }
}

fn coin_vol(item) {
    let v = item.total_volume;
    if type_of(v) == "f64" || type_of(v) == "float" { v }
    else if type_of(v) == "i64" || type_of(v) == "int" { v.to_float() }
    else { 0.0 }
}

fn coin_name(item) {
    let n = item.name;
    if type_of(n) == "string" { n } else { "" }
}

fn make_row_for(item, prev_idx, rank) {
    let sym_raw = item.symbol;
    let sym = sym_raw.to_upper();
    let price = coin_price(item);
    let pct = coin_pct(item);
    let vol = coin_vol(item);
    let hist = [];
    if prev_idx.contains(sym) {
        hist = roll_history(prev_idx[sym].history, price);
    } else {
        hist = [price, price, price, price, price, price, price, price];
    }
    let r = build_row(sym, price, pct, vol, hist);
    r.name = coin_name(item);
    r.rank = "#" + rank;
    r
}

fn on_fetch(tag, body) {
    if tag != "coingecko" { return; }
    let parsed = parse_json(body);
    let prev_idx = index_prev();
    let next = [];
    let total_cap = 0.0;
    let gainers = 0;
    let losers = 0;
    let j = 0;
    while j < parsed.len() {
        let r = make_row_for(parsed[j], prev_idx, j + 1);
        next.push(r);
        let it = parsed[j];
        let m = it.market_cap;
        if type_of(m) == "f64" || type_of(m) == "float" { total_cap += m; }
        else if type_of(m) == "i64" || type_of(m) == "int" { total_cap += m.to_float(); }
        let p = coin_pct(it);
        if p > 0.0 { gainers += 1; }
        else if p < 0.0 { losers += 1; }
        j += 1;
    }
    signal_array("rows").set(next);
    rebuild_visible();
    let refr = signal("refresh_count", 0).get() + 1;
    signal("refresh_count", 0).set(refr);
    signal("updated_label", "").set("refresh #" + refr);
    signal("feed_status", "").set("live - " + next.len() + " coins");
    signal("cap_label", "").set(fmt_cap(total_cap));
    signal("cap_sub", "").set("total cap, top " + next.len());
    signal("gainers_label", "").set("" + gainers);
    signal("losers_label", "").set("" + losers);
}

fn fmt_cap(v) {
    if v >= 1000000000000.0 {
        let m = (v / 100000000000.0).to_int();
        let whole = m / 10;
        let r = m % 10;
        "$" + whole + "." + r + "T"
    } else if v >= 1000000000.0 {
        let m = (v / 100000000.0).to_int();
        let whole = m / 10;
        let r = m % 10;
        "$" + whole + "." + r + "B"
    } else {
        "$" + v.to_int()
    }
}

fn on_fetch_error(tag, msg) {
    if tag != "coingecko" { return; }
    signal("feed_status", "").set("feed error: " + msg);
    signal("updated_label", "").set("feed error");
}

fn do_fetch() {
    signal("feed_status", "").set("fetching...");
    fetch("https://api.coingecko.com/api/v3/coins/markets?vs_currency=usd&order=market_cap_desc&per_page=80&page=1&sparkline=false&price_change_percentage=24h", "coingecko");
}

fn handle_refresh(id) { do_fetch(); }

fn handle_pause(id) {
    let p = signal("paused", "");
    let was = p.get();
    if was == "1" {
        p.set("");
        signal("pause_label", "").set("Pause auto-refresh");
    } else {
        p.set("1");
        signal("pause_label", "").set("Resume auto-refresh");
    }
}

fn handle_clear(id) {
    signal_array("rows").set([]);
    signal_array("visible").set([]);
    signal("empty_visible", "").set("1");
    refresh_counts(0, 0);
}

fn handle_tick(name) {
    let p = signal("paused", "").get();
    if p == "1" { return; }
    do_fetch();
}

fn handle_search(id, text) {
    if id == "search" {
        signal("filter", "").set(text);
        rebuild_visible();
    }
}

fn on_start() {
    signal("filter", "").set("");
    signal("paused", "").set("");
    signal("pause_label", "").set("Pause auto");
    signal("refresh_count", 0).set(0);
    signal("rows_label", "").set("0");
    signal("ticks_label", "").set("auto-refresh - 30s");
    signal("updated_label", "").set("idle");
    signal("feed_status", "").set("loading...");
    signal("empty_visible", "").set("");
    signal("cap_label", "").set("-");
    signal("cap_sub", "").set("");
    signal("gainers_label", "").set("-");
    signal("losers_label", "").set("-");

    on("click", "refresh", "handle_refresh");
    on("click", "pause",   "handle_pause");
    on("click", "clear",   "handle_clear");
    on("text_input", "search", "handle_search");
    on("timer", "tick", "handle_tick");

    do_fetch();
    set_interval("tick", 30000);
}
