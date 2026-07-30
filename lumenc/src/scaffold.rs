//! Built-in templates for `lumenc new <template> <name>`.
//!
//! Each template is a slice of `(relative_path, file_body)` tuples. The
//! scaffolder walks the slice, creating subdirectories on demand and
//! writing each body verbatim. Templates are starters, not full demos -
//! but every one runs out of the box (`lumenc run <name>`) and each ships
//! a README explaining the concepts it demonstrates.
//!
//! The [`TEMPLATES`] registry drives `lumenc new --list` and the template
//! lookup in `cmd_new`; the per-template constants stay public for direct
//! embedding (tests, docs generators).

/// One gallery entry: name, one-line description, and the file set.
pub struct Template {
    /// CLI name (`lumenc new <name> <dir>`).
    pub name: &'static str,
    /// One-line description shown by `lumenc new --list`.
    pub description: &'static str,
    /// `(relative_path, body)` tuples written verbatim.
    pub files: &'static [(&'static str, &'static str)],
}

/// Every built-in template, in gallery order (simplest first).
pub const TEMPLATES: &[Template] = &[
    Template {
        name: "hello",
        description: "Smallest runnable app: one label + a script that says hi.",
        files: HELLO,
    },
    Template {
        name: "counter",
        description: "Click-to-bump counter: buttons, bind-text, per-id click routing.",
        files: COUNTER,
    },
    Template {
        name: "form",
        description: "Two-way bound form: input, toggle, slider, live status line.",
        files: FORM,
    },
    Template {
        name: "todo",
        description: "The canonical tutorial app: list + input + <for> loop + array signals.",
        files: TODO,
    },
    Template {
        name: "dashboard",
        description: "Stat tiles + progress bars + activity feed, driven by a timer.",
        files: DASHBOARD,
    },
    Template {
        name: "settings",
        description: "Settings panel: checkbox / radio / dropdown / slider groups + derive().",
        files: SETTINGS,
    },
    Template {
        name: "hotkeys",
        description: "Native shell showcase: global hotkeys, tray icon, OS notifications.",
        files: HOTKEYS,
    },
];

/// Look up a template by CLI name.
pub fn find(name: &str) -> Option<&'static Template> {
    TEMPLATES.iter().find(|t| t.name == name)
}

/// Comma-separated template names for error messages / usage text.
pub fn template_names() -> String {
    TEMPLATES
        .iter()
        .map(|t| t.name)
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Smallest runnable app: a single label + script that says hi.
pub const HELLO: &[(&str, &str)] = &[
    (
        "main.lmn",
        r##"<root bg="#0c1c30">
  <label width="100%" height="100%" text-align="center" font-size="48"
         text-color="#ffffff" text="Hello, Lumen" />
  <script src="main.rhai" />
</root>
"##,
    ),
    (
        "main.rhai",
        "fn on_start() {\n    print(\"hello from lumen\");\n}\n",
    ),
    (
        "lumen.toml",
        "[app]\nentry = \"main.lmn\"\n\n[window]\ntitle = \"Hello\"\nsize = [640, 360]\n",
    ),
    (
        "README.md",
        r##"# hello

The smallest runnable Lumen app.

Concepts demonstrated:

- **`main.lmn`** - every app is one `<root>` element; a `<label>` fills it.
- **`<script src="main.rhai" />`** - attaches a Rhai script; `on_start()`
  runs once after the first layout.
- **`lumen.toml`** - window title + logical size.

Run it:

```sh
lumenc run .
```
"##,
    ),
];

/// Click-to-bump counter. Demonstrates `<button>` + `bind-text` +
/// per-id `on(\"click\", \"...\", \"...\")` routing + signal handles.
pub const COUNTER: &[(&str, &str)] = &[
    (
        "main.lmn",
        r##"<root bg="#0c1c30" padding="32" gap="20" align="center" justify="center">
  <label class="display" id="counter" width="100%" height="120px" text="0"
         bind-text="clicks" />
  <row gap="14" justify="center">
    <button class="primary" id="bump"  width="120px" height="48px" text="+1" />
    <button class="primary" id="reset" width="120px" height="48px" text="reset" />
  </row>
  <script src="main.rhai" />
</root>
"##,
    ),
    (
        "main.css",
        r##":root {
  --color-accent:  #5fd9e0;
  --color-bg:      #163459;
  --color-hover:   #1d4477;
  --color-active:  #0e2c52;
  --color-on-bg:   #ffffff;
  --radius-pill:   24;
}

.display { text-align: center; font-size: 96; text-color: var(--color-on-bg); }

.primary {
  bg:        var(--color-bg);
  hover-bg:  var(--color-hover);
  press-bg:  var(--color-active);
  text-color: var(--color-on-bg);
  radius:    var(--radius-pill);
  text-align: center;
  font-size: 18;
}
.primary:focus { outline: 2 var(--color-accent); }
"##,
    ),
    (
        "main.rhai",
        r##"fn on_start() {
    on("click", "bump",  "handle_bump");
    on("click", "reset", "handle_reset");
}

fn handle_bump(id) {
    let n = signal("clicks", 0);
    n.set(n.get() + 1);
}

fn handle_reset(id) {
    signal("clicks", 0).set(0);
}
"##,
    ),
    (
        "lumen.toml",
        "[app]\nentry = \"main.lmn\"\n\n[window]\ntitle = \"Counter\"\nsize = [480, 360]\n",
    ),
    (
        "README.md",
        r##"# counter

The classic click counter.

Concepts demonstrated:

- **Signals** - `signal("clicks", 0)` returns a typed handle; `.get()` /
  `.set(v)` round-trip through the reactive store.
- **`bind-text="clicks"`** - the label re-renders whenever the signal
  changes; no manual `set_text` calls.
- **Per-id click routing** - `on("click", "bump", "handle_bump")` beats
  a global `on_click(id)` dispatch for readability.
- **CSS custom properties** - every color / radius lives in `:root` vars,
  so a theme swap touches one block.

Run it:

```sh
lumenc run .
```
"##,
    ),
];

/// Form-style app: input + toggle + slider, all two-way bound.
/// A starting point for settings panels, login screens, etc.
pub const FORM: &[(&str, &str)] = &[
    (
        "main.lmn",
        r##"<root bg="#0c1c30" padding="32" gap="18">
  <label class="title" width="100%" height="44px" text="Profile" />

  <column gap="6">
    <label class="caption" text="Display name" />
    <input id="name" class="field" tab-index="0" width="100%" height="40px"
           placeholder="What should we call you?" bind-text="name" />
  </column>

  <row gap="12" align="center">
    <toggle id="dark" class="control" tab-index="1" width="48px" height="28px"
            bind-checked="dark" />
    <label class="caption" text="Dark mode" />
  </row>

  <column gap="6">
    <label class="caption" text="Volume" />
    <slider id="volume" class="control" tab-index="2" width="100%" height="28px"
            min="0" max="1" value="0.5" bind-value="volume" />
  </column>

  <label class="echo" width="100%" height="40px" bind-text="status"
         text="(waiting for input...)" />

  <script src="main.rhai" />
</root>
"##,
    ),
    (
        "main.css",
        r##":root {
  --color-accent: #5fd9e0;
  --color-bg:     #163459;
  --color-hover:  #1d4477;
  --color-on-bg:  #ffffff;
  --color-on-bg-dim: #8eaecf;
  --radius-md:    14;
}

.title   { font-size: 28; text-color: var(--color-on-bg); }
.caption { font-size: 13; text-color: var(--color-on-bg-dim); }
.echo    { font-size: 14; text-color: var(--color-on-bg-dim); }

.field {
  bg:        var(--color-bg);
  hover-bg:  var(--color-hover);
  text-color: var(--color-on-bg);
  radius:    var(--radius-md);
  padding:   0 14;
  font-size: 15;
}
.field:focus { outline: 2 var(--color-accent); }
.control:focus { outline: 2 var(--color-accent); }
"##,
    ),
    (
        "main.rhai",
        r##"fn refresh_status() {
    let name = signal("name", "").get();
    let dark = signal("dark", "false").get();
    let vol  = signal("volume", "0.5").get();
    let theme = if dark == "true" { "dark" } else { "light" };
    signal("status", "")
        .set("Hi " + name + " - theme=" + theme + " volume=" + vol);
}

fn on_start() {
    refresh_status();
}

fn on_text_input(id, text) { refresh_status(); }
fn on_toggle(id, on)       { refresh_status(); }
fn on_slider(id, value)    { refresh_status(); }
"##,
    ),
    (
        "lumen.toml",
        "[app]\nentry = \"main.lmn\"\n\n[window]\ntitle = \"Form\"\nsize = [560, 420]\n",
    ),
    (
        "README.md",
        r##"# form

A two-way bound form: every control mirrors into a signal, and the
status line re-derives from the signals on each edit.

Concepts demonstrated:

- **Two-way bindings** - `bind-text` (input), `bind-checked` (toggle),
  `bind-value` (slider). User edits write back into the signals.
- **Lifecycle callbacks** - `on_text_input(id, text)`, `on_toggle(id, on)`,
  `on_slider(id, value)` fire on every control change.
- **Focus styling** - `tab-index` puts controls in the Tab chain;
  `:focus { outline: ... }` shows the ring.

Run it:

```sh
lumenc run .
```
"##,
    ),
];

/// Todo list: the canonical tutorial app. Array signals + `<for>` loop +
/// per-row action buttons routed through the global `on_click` fallback.
pub const TODO: &[(&str, &str)] = &[
    (
        "main.lmn",
        r##"<root bg="#0c1c30" padding="28" gap="16">
  <label class="title" height="40px" text="Todo" />

  <row gap="10">
    <input id="draft" class="field" grow="1" height="40px" tab-index="0"
           placeholder="What needs doing?" bind-text="draft" />
    <button id="add" class="primary" width="96px" height="40px" text="Add" />
  </row>

  <label class="caption" height="22px" bind-text="left_label" text="" />

  <scroll grow="1" gap="0">
    <for each="todos" key="id" gap="6">
      <row class="todo-row" align="center" gap="10" height="44px">
        <button id="tg|{row.id}" class="check {row.check_cls}" width="30px" height="30px"
                text="{row.mark}" />
        <label class="{row.label_cls}" grow="1" wrap="none" max-lines="1"
               text="{row.label}" />
        <button id="rm|{row.id}" class="remove" width="34px" height="30px" text="✕" />
      </row>
    </for>
  </scroll>

  <row gap="10">
    <button id="clear-done" class="ghost" width="140px" height="36px" text="Clear done" />
  </row>

  <script src="main.rhai" />
</root>
"##,
    ),
    (
        "main.css",
        r##":root {
  --color-accent:   #5fd9e0;
  --color-bg:       #163459;
  --color-hover:    #1d4477;
  --color-active:   #0e2c52;
  --color-row:      #10263f;
  --color-on-bg:    #ffffff;
  --color-on-bg-dim:#8eaecf;
  --color-done:     #5a7a99;
  --color-danger:   #e06c75;
  --radius-md:      10;
}

.title   { font-size: 26; text-color: var(--color-on-bg); }
.caption { font-size: 12; text-color: var(--color-on-bg-dim); }

.field {
  bg: var(--color-bg);
  hover-bg: var(--color-hover);
  text-color: var(--color-on-bg);
  radius: var(--radius-md);
  padding: 0 14;
  font-size: 15;
}
.field:focus { outline: 2 var(--color-accent); }

.primary {
  bg: var(--color-bg);
  hover-bg: var(--color-hover);
  press-bg: var(--color-active);
  text-color: var(--color-on-bg);
  radius: var(--radius-md);
  text-align: center;
  font-size: 15;
}
.primary:focus { outline: 2 var(--color-accent); }

.ghost {
  bg: var(--color-row);
  hover-bg: var(--color-hover);
  text-color: var(--color-on-bg-dim);
  radius: var(--radius-md);
  text-align: center;
  font-size: 13;
}
.ghost:focus { outline: 2 var(--color-accent); }

.todo-row { bg: var(--color-row); radius: var(--radius-md); padding: 0 10; }

.check {
  bg: var(--color-bg);
  hover-bg: var(--color-hover);
  text-color: var(--color-on-bg-dim);
  radius: 15;
  text-align: center;
  font-size: 14;
}
.check-on { text-color: var(--color-accent); }
.check:focus { outline: 2 var(--color-accent); }

.todo-label      { font-size: 15; text-color: var(--color-on-bg); }
.todo-label-done { font-size: 15; text-color: var(--color-done); }

.remove {
  bg: var(--color-row);
  hover-bg: var(--color-hover);
  text-color: var(--color-danger);
  radius: var(--radius-md);
  text-align: center;
  font-size: 14;
}
.remove:focus { outline: 2 var(--color-accent); }
"##,
    ),
    (
        "main.rhai",
        r##"// Rows live in the "todos" array signal; `<for each="todos" key="id">`
// reconciles the list on every write. Presentation fields (mark,
// label_cls, check_cls) are computed here so the markup stays dumb.

fn decorate(rows) {
    let out = [];
    for r in rows {
        let done = r.done == "true";
        r.mark      = if done { "●" } else { "○" };
        r.check_cls = if done { "check-on" } else { "" };
        r.label_cls = if done { "todo-label-done" } else { "todo-label" };
        out.push(r);
    }
    out
}

fn write_rows(rows) {
    signal_array("todos").set(decorate(rows));
    let left = 0;
    for r in rows { if r.done != "true" { left += 1; } }
    let total = rows.len();
    signal("left_label", "").set(left + " open / " + total + " total");
}

fn on_start() {
    on("click", "add", "add_todo");
    on("click", "clear-done", "clear_done");
    signal("next_id", 4);
    write_rows([
        #{ id: "1", label: "Scaffold this app",        done: "true"  },
        #{ id: "2", label: "Read the todo README",     done: "false" },
        #{ id: "3", label: "Build something reactive", done: "false" },
    ]);
}

fn add_todo(id) {
    let draft = signal("draft", "").get();
    if draft == "" { return; }
    let n = signal("next_id", 4);
    let rows = signal_array("todos").all();
    rows.push(#{ id: "" + n.get(), label: draft, done: "false" });
    n.set(n.get() + 1);
    signal("draft", "").set("");
    write_rows(rows);
}

fn clear_done(id) {
    let rows = signal_array("todos").all();
    rows.retain(|r| r.done != "true");
    write_rows(rows);
}

// Per-row buttons carry interpolated ids ("tg|<id>", "rm|<id>");
// the global on_click fallback parses the prefix.
fn on_click(id) {
    if id.starts_with("tg|") { toggle_todo(id.sub_string(3)); return; }
    if id.starts_with("rm|") { remove_todo(id.sub_string(3)); return; }
}

fn toggle_todo(tid) {
    let rows = signal_array("todos").all();
    for i in 0..rows.len() {
        if rows[i].id == tid {
            let r = rows[i];
            r.done = if r.done == "true" { "false" } else { "true" };
            rows[i] = r;
        }
    }
    write_rows(rows);
}

fn remove_todo(tid) {
    let rows = signal_array("todos").all();
    rows.retain(|r| r.id != tid);
    write_rows(rows);
}
"##,
    ),
    (
        "lumen.toml",
        "[app]\nentry = \"main.lmn\"\n\n[window]\ntitle = \"Todo\"\nsize = [520, 640]\n",
    ),
    (
        "README.md",
        r##"# todo

The canonical tutorial app: a reactive list with add / toggle / remove.

Concepts demonstrated:

- **Array signals** - `signal_array("todos")` holds an ordered list of
  record maps; `.all()` snapshots it, `.set(rows)` writes it back.
- **`<for each="todos" key="id">`** - one row subtree per item, diffed by
  the `key` field so focus / scroll survive edits. Row fields are read
  with the `{row.field}` form.
- **Per-row actions** - buttons interpolate the row id into their own id
  (`id="rm|{row.id}"`); the global `on_click(id)` fallback parses the
  prefix. Compare with the per-id `on("click", ...)` routing used for
  the static Add button.
- **Derived presentation** - `mark` / `label_cls` / `check_cls` are
  computed in the script (`decorate`), keeping markup declarative.
- **`<scroll>`** - the list scrolls independently; try adding 30 rows.

Run it:

```sh
lumenc run .
```
"##,
    ),
];

/// Dashboard: stat tiles + progress bars + activity list, all driven by a
/// repeating timer so the UI visibly updates without user input.
pub const DASHBOARD: &[(&str, &str)] = &[
    (
        "main.lmn",
        r##"<root bg="#0b1420" padding="24" gap="16">
  <row align="center" gap="12" height="40px">
    <label class="title" text="Ops dashboard" />
    <spacer />
    <label class="caption" bind-text="clock" text="tick 0" />
    <button id="pause" class="ghost" width="96px" height="32px" bind-text="pause_label" text="Pause" />
  </row>

  <row gap="12" height="110px">
    <column class="stat" grow="1">
      <label class="stat-label" text="Requests / min" />
      <label class="stat-value" height="52px" bind-text="requests" text="0" />
    </column>
    <column class="stat" grow="1">
      <label class="stat-label" text="Active users" />
      <label class="stat-value" height="52px" bind-text="users" text="0" />
    </column>
    <column class="stat" grow="1">
      <label class="stat-label" text="Error rate" />
      <label class="stat-value accent" height="52px" bind-text="errors" text="0%" />
    </column>
  </row>

  <column class="card" gap="10" padding="16">
    <row gap="10" align="center">
      <label class="caption" width="90px" text="CPU" />
      <progress grow="1" height="10px" bind-value="cpu" max="100" />
      <label class="caption" width="48px" text-align="end" bind-text="cpu_label" text="0%" />
    </row>
    <row gap="10" align="center">
      <label class="caption" width="90px" text="Memory" />
      <progress grow="1" height="10px" bind-value="mem" max="100" />
      <label class="caption" width="48px" text-align="end" bind-text="mem_label" text="0%" />
    </row>
  </column>

  <label class="caption" height="20px" text="Recent activity" />
  <scroll grow="1">
    <for each="activity" key="id" gap="4">
      <row class="feed-row" align="center" gap="10" height="34px">
        <label class="feed-time" width="70px" text="{row.time}" />
        <label class="feed-text" grow="1" wrap="none" max-lines="1" text="{row.text}" />
      </row>
    </for>
  </scroll>

  <script src="main.rhai" />
</root>
"##,
    ),
    (
        "main.css",
        r##":root {
  --color-accent:   #5fd9e0;
  --color-card:     #101d2e;
  --color-row:      #0e1927;
  --color-hover:    #1d4477;
  --color-on-bg:    #ffffff;
  --color-on-bg-dim:#7f96ad;
  --radius-md:      12;
}

.title   { font-size: 22; text-color: var(--color-on-bg); }
.caption { font-size: 12; text-color: var(--color-on-bg-dim); }

.ghost {
  bg: var(--color-card);
  hover-bg: var(--color-hover);
  text-color: var(--color-on-bg-dim);
  radius: var(--radius-md);
  text-align: center;
  font-size: 13;
}
.ghost:focus { outline: 2 var(--color-accent); }

.card { bg: var(--color-card); radius: var(--radius-md); }

.stat { bg: var(--color-card); radius: var(--radius-md); padding: 14; gap: 8; }
.stat-label { font-size: 12; text-color: var(--color-on-bg-dim); }
.stat-value { font-size: 34; text-color: var(--color-on-bg); }
.stat-value.accent { text-color: var(--color-accent); }

progress { bg: var(--color-row); radius: 5; }
.progress-fill { bg: var(--color-accent); radius: 5; }

.feed-row  { bg: var(--color-row); radius: 8; padding: 0 10; }
.feed-time { font-size: 12; text-color: var(--color-on-bg-dim); }
.feed-text { font-size: 13; text-color: var(--color-on-bg); }
"##,
    ),
    (
        "main.rhai",
        r##"// A repeating timer walks the metrics through a deterministic
// pseudo-random sequence (LCG), so the dashboard animates without any
// backend. Swap `step()` for `fetch(...)` + `on_fetch` to drive it
// from a real API.

fn rand(n) {
    // Park-Miller LCG over a signal-backed seed.
    let s = signal("seed", 20260722);
    let next = (s.get() * 48271) % 2147483647;
    s.set(next);
    next % n
}

fn step() {
    let tick = signal("ticks", 0);
    tick.set(tick.get() + 1);
    signal("clock", "").set("tick " + tick.get());

    let req = 180 + rand(120);
    let usr = 40 + rand(25);
    let err = rand(40);
    signal("requests", 0).set(req);
    signal("users", 0).set(usr);
    signal("errors", "").set((err / 10) + "." + (err % 10) + "%");

    let cpu = 20 + rand(70);
    let mem = 35 + rand(50);
    signal("cpu", 0).set(cpu);
    signal("mem", 0).set(mem);
    signal("cpu_label", "").set(cpu + "%");
    signal("mem_label", "").set(mem + "%");

    let feed = signal_array("activity");
    let n = tick.get();
    feed.push(#{
        id: "" + n,
        time: "+" + n + "s",
        text: "deploy " + (1000 + rand(9000)) + " served " + req + " req/min",
    });
    // Keep the feed bounded (newest last).
    let rows = feed.all();
    if rows.len() > 12 {
        rows.remove(0);
        feed.set(rows);
    }
}

fn on_start() {
    on("click", "pause", "toggle_pause");
    signal("pause_label", "").set("Pause");
    step();
    set_interval("sim", 1200);
}

fn on_timer(name) {
    if name == "sim" { step(); }
}

fn toggle_pause(id) {
    let running = signal("running", "true");
    if running.get() == "true" {
        cancel_timer("sim");
        running.set("false");
        signal("pause_label", "").set("Resume");
    } else {
        set_interval("sim", 1200);
        running.set("true");
        signal("pause_label", "").set("Pause");
    }
}
"##,
    ),
    (
        "lumen.toml",
        "[app]\nentry = \"main.lmn\"\n\n[window]\ntitle = \"Dashboard\"\nsize = [780, 620]\n",
    ),
    (
        "README.md",
        r##"# dashboard

A live metrics dashboard: stat tiles, progress bars, and an activity
feed, all animated by a repeating timer.

Concepts demonstrated:

- **`<tile>` composition** - stat cards are plain tiles with labels;
  every color / radius routes through CSS custom properties.
- **`<progress bind-value max>`** - determinate bars track a numeric
  signal; the fill is styled via `.progress-fill { ... }`.
- **Timers** - `set_interval("sim", 1200)` + `on_timer(name)` drive the
  simulation; `cancel_timer` pauses it (Pause / Resume button).
- **Bounded array feeds** - the activity list pushes onto an array
  signal and trims the head so the `<for>` stays 12 rows.
- **`bind-text` everywhere** - no `set_text` calls; every dynamic string
  is a signal.

Swap the `step()` simulation for `fetch(url, tag)` + `on_fetch` to feed
the same UI from a real endpoint.

Run it:

```sh
lumenc run .
```
"##,
    ),
];

/// Settings panel: checkbox / radio / dropdown / slider groups with a
/// derive()-computed summary. The forms pattern.
pub const SETTINGS: &[(&str, &str)] = &[
    (
        "main.lmn",
        r##"<root bg="#0c1c30" padding="28" gap="18">
  <label class="title" height="40px" text="Settings" />

  <column class="group" gap="10" padding="16">
    <label class="group-title" text="Appearance" />
    <row gap="10" align="center">
      <label class="caption" width="120px" text="Theme" />
      <dropdown bind-value="theme">
        <option value="dark"   label="Dark" />
        <option value="light"  label="Light" />
        <option value="system" label="Follow system" />
      </dropdown>
    </row>
    <column gap="4">
      <label class="caption" text="Density" />
      <radio group="density" value="compact"     label="Compact" />
      <radio group="density" value="comfortable" label="Comfortable" checked="true" />
      <radio group="density" value="spacious"    label="Spacious" />
    </column>
    <row gap="10" align="center">
      <label class="caption" width="120px" text="UI scale" />
      <slider id="scale" grow="1" height="28px" min="0.8" max="1.6" value="1.0"
              step="0.1" bind-value="scale" />
      <label class="caption" width="40px" text-align="end" bind-text="scale_label" text="1.0" />
    </row>
  </column>

  <column class="group" gap="8" padding="16">
    <label class="group-title" text="Notifications" />
    <checkbox id="email" label="Email digests"       bind-checked="notify_email" />
    <checkbox id="push"  label="Push notifications"  bind-checked="notify_push" checked="true" />
    <checkbox id="sound" label="Sounds"              bind-checked="notify_sound" />
    <row gap="12" align="center">
      <toggle id="dnd" width="48px" height="28px" bind-checked="dnd" />
      <label class="caption" text="Do not disturb" />
    </row>
  </column>

  <label class="summary" height="60px" wrap="word" bind-text="summary" text="" />

  <script src="main.rhai" />
</root>
"##,
    ),
    (
        "main.css",
        r##":root {
  --color-accent:   #5fd9e0;
  --color-bg:       #163459;
  --color-hover:    #1d4477;
  --color-group:    #10263f;
  --color-on-bg:    #ffffff;
  --color-on-bg-dim:#8eaecf;
  --radius-md:      12;
}

.title       { font-size: 26; text-color: var(--color-on-bg); }
.group       { bg: var(--color-group); radius: var(--radius-md); }
.group-title { font-size: 15; text-color: var(--color-accent); }
.caption     { font-size: 13; text-color: var(--color-on-bg-dim); }
.summary     { font-size: 13; text-color: var(--color-on-bg-dim); }

checkbox:checked { bg: var(--color-accent); }
checkbox:focus   { outline: 2 var(--color-accent); }
radio:selected   { bg: var(--color-accent); }
radio:focus      { outline: 2 var(--color-accent); }
toggle:focus     { outline: 2 var(--color-accent); }
slider:focus     { outline: 2 var(--color-accent); }
"##,
    ),
    (
        "main.rhai",
        r##"// Every control writes a signal; `derive` recomputes the summary from
// the full set whenever ANY of them changes - no per-control wiring.

fn on_start() {
    signal("theme", "").set("dark");
    derive("summary",
        ["theme", "density", "scale", "notify_email", "notify_push",
         "notify_sound", "dnd"],
        |theme, density, scale, email, push, sound, dnd| {
            // Deps may be unset on the very first evaluation (the
            // on_start writes above commit at the next tick boundary) -
            // fall back to the authored defaults for display.
            if theme == "" { theme = "dark"; }
            if scale == "" { scale = "1.0"; }
            let chan = "";
            if email == "true" { chan += "email "; }
            if push  == "true" { chan += "push "; }
            if sound == "true" { chan += "sound "; }
            if chan == "" { chan = "none"; }
            let quiet = if dnd == "true" { " - do not disturb" } else { "" };
            "Theme " + theme + " · density " + density + " · scale " + scale
                + " · notify via " + chan + quiet
        });
}

fn on_slider(id, value) {
    if id == "scale" {
        // Round to one decimal for the caption.
        let tenths = (value * 10.0).round() / 10.0;
        signal("scale_label", "").set("" + tenths);
    }
}
"##,
    ),
    (
        "lumen.toml",
        "[app]\nentry = \"main.lmn\"\n\n[window]\ntitle = \"Settings\"\nsize = [560, 660]\n",
    ),
    (
        "README.md",
        r##"# settings

The forms pattern: grouped controls, each bound to a signal, with a
`derive()`-computed summary line.

Concepts demonstrated:

- **`<checkbox label bind-checked>`** - box + caption in one tag;
  clicking anywhere on the row toggles.
- **`<radio group value label>`** - exclusive groups; the selected value
  lives in the signal named by `group` (here `density`). Arrow keys move
  the selection; the group is one Tab stop.
- **`<dropdown bind-value>` + `<option>`** - select widget; `Escape` or
  an outside click dismisses the panel.
- **`<slider min max step bind-value>`** - keyboard-drivable numeric
  input.
- **`derive(name, deps, fn)`** - the summary recomputes when any dep
  changes; one declaration replaces seven callbacks.
- **State pseudo-classes** - `checkbox:checked`, `radio:selected`,
  `:focus` outlines, all in CSS.

Run it:

```sh
lumenc run .
```
"##,
    ),
];

/// Native shell showcase: global hotkeys, a tray icon, and OS
/// notifications, with an in-app event log.
pub const HOTKEYS: &[(&str, &str)] = &[
    (
        "main.lmn",
        r##"<root bg="#0c1c30" padding="28" gap="16">
  <label class="title" height="40px" text="Native shell" />
  <label class="caption" wrap="word"
         text="Global hotkeys fire even while another app has focus. The tray icon (macOS / Windows) and OS notifications use the same shell surface." />

  <column class="group" gap="8" padding="16">
    <label class="group-title" text="Registered hotkeys" />
    <row gap="10" align="center">
      <label class="kbd" width="220px" text="Ctrl/Cmd + Shift + L" />
      <label class="caption" text="log a line" />
    </row>
    <row gap="10" align="center">
      <label class="kbd" width="220px" text="Ctrl/Cmd + Shift + K" />
      <label class="caption" text="send a notification" />
    </row>
    <row gap="12" align="center">
      <toggle id="armed" width="48px" height="28px" checked="true" bind-checked="armed" />
      <label class="caption" text="Hotkeys armed" />
    </row>
  </column>

  <row gap="10">
    <button id="notify-now" class="primary" width="180px" height="40px"
            text="Test notification" />
    <button id="clear-log" class="ghost" width="120px" height="40px" text="Clear log" />
  </row>

  <label class="caption" height="20px" bind-text="log_label" text="Event log" />
  <scroll grow="1">
    <for each="log" key="id" gap="4">
      <row class="log-row" align="center" gap="10" height="32px">
        <label class="log-kind" width="90px" text="{row.kind}" />
        <label class="log-text" grow="1" wrap="none" max-lines="1" text="{row.text}" />
      </row>
    </for>
  </scroll>

  <script src="main.rhai" />
</root>
"##,
    ),
    (
        "main.css",
        r##":root {
  --color-accent:   #5fd9e0;
  --color-bg:       #163459;
  --color-hover:    #1d4477;
  --color-active:   #0e2c52;
  --color-group:    #10263f;
  --color-row:      #0e1927;
  --color-on-bg:    #ffffff;
  --color-on-bg-dim:#8eaecf;
  --radius-md:      12;
}

.title       { font-size: 26; text-color: var(--color-on-bg); }
.caption     { font-size: 13; text-color: var(--color-on-bg-dim); }
.group       { bg: var(--color-group); radius: var(--radius-md); }
.group-title { font-size: 15; text-color: var(--color-accent); }

.kbd {
  font-size: 13;
  text-color: var(--color-accent);
  bg: var(--color-row);
  radius: 6;
  padding: 4 10;
  text-align: center;
}

.primary {
  bg: var(--color-bg);
  hover-bg: var(--color-hover);
  press-bg: var(--color-active);
  text-color: var(--color-on-bg);
  radius: var(--radius-md);
  text-align: center;
  font-size: 15;
}
.primary:focus { outline: 2 var(--color-accent); }

.ghost {
  bg: var(--color-group);
  hover-bg: var(--color-hover);
  text-color: var(--color-on-bg-dim);
  radius: var(--radius-md);
  text-align: center;
  font-size: 13;
}
.ghost:focus { outline: 2 var(--color-accent); }

.log-row  { bg: var(--color-row); radius: 8; padding: 0 10; }
.log-kind { font-size: 12; text-color: var(--color-accent); }
.log-text { font-size: 13; text-color: var(--color-on-bg); }
"##,
    ),
    (
        "main.rhai",
        r##"// Global hotkeys (global-hotkey crate under the hood), a tray icon
// (macOS / Windows; Linux logs a warning and skips), and OS
// notifications (notify-rust). Everything lands in the in-app log so
// the demo is observable even when notifications are unavailable.

fn log_event(kind, text) {
    let n = signal("log_n", 0);
    n.set(n.get() + 1);
    let log = signal_array("log");
    log.push(#{ id: "" + n.get(), kind: kind, text: text });
    let rows = log.all();
    if rows.len() > 20 {
        rows.remove(0);
        log.set(rows);
    }
    signal("log_label", "").set("Event log (" + rows.len() + ")");
}

fn arm() {
    register_hotkey("log-line", "CommandOrControl+Shift+L");
    register_hotkey("notify",   "CommandOrControl+Shift+K");
}

fn disarm() {
    unregister_hotkey("log-line");
    unregister_hotkey("notify");
}

fn on_start() {
    on("click", "notify-now", "send_notification");
    on("click", "clear-log", "clear_log");
    arm();
    // Tray icons need a real icon asset; ship one at icons/tray.png to
    // light this up on macOS / Windows. Linux logs a warning and skips.
    tray_icon("main", "icons/tray.png", "Lumen hotkeys demo");
    log_event("ready", "hotkeys armed - try Ctrl/Cmd+Shift+L from another window");
}

fn on_hotkey(name) {
    if name == "log-line" {
        log_event("hotkey", "Ctrl/Cmd+Shift+L pressed");
    }
    if name == "notify" {
        log_event("hotkey", "Ctrl/Cmd+Shift+K pressed - sending notification");
        notify("Lumen hotkeys", "Global hotkey fired.");
    }
}

fn on_tray(id) {
    log_event("tray", "tray icon clicked (" + id + ")");
}

fn on_toggle(id, checked) {
    if id == "armed" {
        if checked { arm();    log_event("state", "hotkeys armed"); }
        else       { disarm(); log_event("state", "hotkeys disarmed"); }
    }
}

fn send_notification(id) {
    notify("Lumen", "Test notification from the hotkeys template.");
    log_event("notify", "test notification sent");
}

fn clear_log(id) {
    signal_array("log").set([]);
    signal("log_label", "").set("Event log");
}
"##,
    ),
    (
        "lumen.toml",
        "[app]\nentry = \"main.lmn\"\n\n[window]\ntitle = \"Hotkeys\"\nsize = [620, 560]\n",
    ),
    (
        "README.md",
        r##"# hotkeys

Native shell showcase: OS-global hotkeys, a system tray icon, and
desktop notifications - with an in-app event log so everything is
observable even where a shell surface is unavailable.

Concepts demonstrated:

- **`register_hotkey(name, accel)` / `unregister_hotkey(name)`** -
  OS-level accelerators (Electron-style `CommandOrControl+Shift+L`
  strings); `on_hotkey(name)` fires even when the app is unfocused.
  The "Hotkeys armed" toggle registers / unregisters live.
- **`tray_icon(id, icon_path, tooltip)`** - system tray entry
  (macOS / Windows; Linux logs a warning and skips). Clicks fire
  `on_tray(id)`. Ship an icon at `icons/tray.png` to light it up.
- **`notify(title, body)`** - fire-and-forget OS notification via the
  platform daemon.
- **Bounded log feed** - the same array-signal + `<for>` pattern the
  dashboard template uses.

Run it:

```sh
lumenc run .
```
"##,
    ),
];
