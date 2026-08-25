// Lanes: kanban board logic.
//
// Source of truth: the `cards` ArraySignal, each item a map
// #{ id, title, tag, col }.  The three <for> lists (view_backlog /
// view_doing / view_done) are rebuilt from it, filtered by the search
// box, on every mutation.  Per-column counts feed derive()d header
// labels and the board summary.
//
// Note: the Rhai host caps expression/block nesting depth, so inner
// loops and predicates live in their own shallow helper functions
// rather than nesting inside one big function.

fn card_matches(it, ql) {
    if ql == "" { return true; }
    let t = it.title.to_lower();
    t.contains(ql)
}

fn build_view(all, col, ql) {
    let view = [];
    let i = 0;
    while i < all.len() {
        let it = all[i];
        if it.col == col && card_matches(it, ql) {
            let r = #{};
            r.id = it.id;
            r.title = it.title;
            r.tag = it.tag;
            r.tag_class = "tag-" + it.tag;
            view.push(r);
        }
        i += 1;
    }
    view
}

fn refresh_board() {
    let all = signal_array("cards").all();
    let raw = signal("search", "").get();
    let ql  = raw.to_lower();
    let cols = ["backlog", "doing", "done"];
    let c = 0;
    while c < cols.len() {
        let col = cols[c];
        let v = build_view(all, col, ql);
        signal_array("view_" + col).set(v);
        signal("count_" + col, "0").set("" + v.len());
        c += 1;
    }
}

fn find_index(cur, key) {
    let i = 0;
    while i < cur.len() {
        if cur[i].id == key { return i; }
        i += 1;
    }
    -1
}

fn open_add(_id) {
    signal("editor_mode", "add").set("add");
    signal("edit_id", "").set("");
    signal("draft_title", "").set("");
    signal("draft_tag", "feature").set("feature");
    signal("draft_col", "backlog").set("backlog");
    signal("editor_heading", "").set("New card");
    signal("editor_open", "").set("1");
}

fn edit_card(key) {
    let cur = signal_array("cards").all();
    let idx = find_index(cur, key);
    if idx < 0 { return; }
    let it = cur[idx];
    signal("editor_mode", "add").set("edit");
    signal("edit_id", "").set(key);
    signal("draft_title", "").set(it.title);
    signal("draft_tag", "feature").set(it.tag);
    signal("draft_col", "backlog").set(it.col);
    signal("editor_heading", "").set("Edit card");
    signal("editor_open", "").set("1");
}

fn apply_edit(cur, key, title, tag, col) {
    let idx = find_index(cur, key);
    if idx < 0 { return cur; }
    let it = cur[idx];
    it.title = title;
    it.tag = tag;
    it.col = col;
    cur[idx] = it;
    cur
}

fn save_card(_id) {
    let title = signal("draft_title", "").get();
    title.trim();
    if title == "" { signal("editor_open", "").set(""); return; }
    let tag  = signal("draft_tag", "feature").get();
    let col  = signal("draft_col", "backlog").get();
    let mode = signal("editor_mode", "add").get();
    let cards = signal_array("cards");
    let cur = cards.all();

    if mode == "edit" {
        cur = apply_edit(cur, signal("edit_id", "").get(), title, tag, col);
    } else {
        let nid = signal("next_id", 1);
        let idv = nid.get();
        nid.set(idv + 1);
        let r = #{};
        r.id = "c" + idv;
        r.title = title;
        r.tag = tag;
        r.col = col;
        cur.push(r);
    }
    cards.set(cur);
    signal("editor_open", "").set("");
    refresh_board();
}

fn cancel_edit(_id) {
    signal("editor_open", "").set("");
}

fn col_index(col) {
    if col == "backlog" { return 0; }
    if col == "doing" { return 1; }
    return 2;
}

fn move_card(key, dir) {
    let cols = ["backlog", "doing", "done"];
    let cards = signal_array("cards");
    let cur = cards.all();
    let idx = find_index(cur, key);
    if idx < 0 { return; }
    let it = cur[idx];
    let ci = col_index(it.col) + dir;
    if ci < 0 { ci = 0; }
    if ci > 2 { ci = 2; }
    it.col = cols[ci];
    cur[idx] = it;
    cards.set(cur);
    refresh_board();
}

fn drop_card(cur, key) {
    let kept = [];
    let i = 0;
    while i < cur.len() {
        if cur[i].id != key { kept.push(cur[i]); }
        i += 1;
    }
    kept
}

fn delete_card(key) {
    let cards = signal_array("cards");
    let kept = drop_card(cards.all(), key);
    cards.set(kept);
    refresh_board();
}

fn col_from_target(tid) {
    if tid == "lane-backlog" { return "backlog"; }
    if tid == "lane-doing"   { return "doing"; }
    if tid == "lane-done"    { return "done"; }
    ""
}

// In-app drag-and-drop landing point. `target_id` is the lane the card
// was released over; `payload` is the dragged card's id (its
// `drag-payload`). Reassign the card's column and re-append so it lands at
// the bottom of the new lane, then let the reactive board rebuild move it.
fn on_drop(target_id, payload) {
    let col = col_from_target(target_id);
    if col == "" { return; }
    let cards = signal_array("cards");
    let cur = cards.all();
    let idx = find_index(cur, payload);
    if idx < 0 { return; }
    let it = cur[idx];
    if it.col == col { return; }
    it.col = col;
    let kept = drop_card(cur, payload);
    kept.push(it);
    cards.set(kept);
    refresh_board();
}

fn seed_cards() {
    let cards = signal_array("cards");
    let list = [];
    let a = #{}; a.id = "c1"; a.title = "Ship the reconciler fast-path"; a.tag = "feature"; a.col = "done"; list.push(a);
    let b = #{}; b.id = "c2"; b.title = "Popup edge-flip near viewport bottom"; b.tag = "bug"; b.col = "done"; list.push(b);
    let c = #{}; c.id = "c3"; c.title = "Grid track sizing (fr + minmax)"; c.tag = "feature"; c.col = "doing"; list.push(c);
    let d = #{}; d.id = "c4"; d.title = "Retint dropdown panels to app tokens"; d.tag = "design"; d.col = "doing"; list.push(d);
    let e = #{}; e.id = "c5"; e.title = "Wire @media width to live viewport"; e.tag = "feature"; e.col = "backlog"; list.push(e);
    let f = #{}; f.id = "c6"; f.title = "Audit CSS warnings across examples"; f.tag = "chore"; f.col = "backlog"; list.push(f);
    cards.set(list);
}

fn count_label(n) {
    if n == "1" { return "1 card"; }
    n + " cards"
}

fn on_start() {
    signal("search", "").set("");
    signal("editor_open", "").set("");
    signal("next_id", 1).set(7);

    seed_cards();

    derive("hdr_backlog", ["count_backlog"], |n| count_label(n));
    derive("hdr_doing",   ["count_doing"],   |n| count_label(n));
    derive("hdr_done",    ["count_done"],    |n| count_label(n));
    derive("board_summary", ["count_backlog", "count_doing", "count_done"], |b, d, dn|
        b + " queued | " + d + " active | " + dn + " shipped");

    // Live search: `<input bind-text="search">` mirrors every keystroke
    // into the `search` signal on its own (no script hook needed for
    // that part). Lumen is reactive-only, so there is no on_tick to poll
    // it against a "last applied" copy; instead this derive() re-runs
    // refresh_board() whenever `search` changes, which is exactly what
    // derive()'s dependency tracking is for. The derived signal itself
    // is not read anywhere; it exists to carry the refilter side effect.
    derive("search_filter", ["search"], |s| { refresh_board(); s });

    on("click", "new-card",      "open_add");
    on("click", "editor-save",   "save_card");
    on("click", "editor-cancel", "cancel_edit");

    refresh_board();
}

fn on_click(id) {
    if id.starts_with("dl|") { delete_card(id.sub_string(3)); return; }
    if id.starts_with("ml|") { move_card(id.sub_string(3), -1); return; }
    if id.starts_with("mr|") { move_card(id.sub_string(3), 1); return; }
    if id.starts_with("ed|") { edit_card(id.sub_string(3)); return; }
}
