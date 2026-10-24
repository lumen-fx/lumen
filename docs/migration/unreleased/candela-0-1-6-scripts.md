# candela scripts follow candela 0.1.6: `is` and `as`, `string(x)`, map brackets, module names

This affects every app with candela scripts, in `.cdl` files or inline
`<script>` blocks. Lumen now links candela 0.1.6, and a script written for the
candela Lumen linked before fails to compile where it uses a spelling 0.1.6
removed. Each removed spelling is a compile error whose help names the
replacement.

The changes most app scripts meet:

| Before | After |
| --- | --- |
| `str(x)` | `string(x)` |
| `as_map(v)`, `as_list(v)` | `v as {string: any}`, `v as any[]` |
| `as_str(v)`, `as_int(v)`, `as_float(v)`, `as_bool(v)` | `v as string`, `v as int`, `v as float`, `v as bool` |
| `is_int(v)` and its siblings | `v is int`, `v is string`, `v is any[]`, ... |
| `is_null(v)` | `v == null` |
| `m.get(k)` for a key that has to be there | `m[k]` |
| `m.insert(k, v)` | `m[k] = v` |
| `S { a: 1, ..Default::default() }` | `S { a: 1, .. }` |
| `Default::default()` for a whole struct | `S::default()`, such as `process::StartOptions::default()` |
| `json_parse(s)`, `json_stringify(v)` | `json::parse(s)`, `json::stringify(v)` after `import "std/json";` |
| `now()` after `import "std/time";` | `time::now()` |

`as` and `is` bind like `<`, looser than `+`, so a downcast inside a sum or a
string join needs parentheses: `"n=" + (v as string)`.

`m.get(k)` still exists and now returns an `Option`: `Some` of the value, or
`None` for a missing key. An `import "std/..."` binds the module's name rather
than merging its functions into the script, so `import "std/math";` is followed
by `math::sqrt(x)`, not `sqrt(x)`. `import "lumen.cdl";` is unchanged: the
`lumen::` builtins, `signal<T>(...)`, `get_by_id` and the rest stay as they
were.

Before:

```rust
import "lumen.cdl";

fn on_http(tag: string, response: any) {
    let r = as_map(response);
    if as_bool(r.get("ok")) {
        lumen::signal_set("status", "got " + str(as_str(r.get("body")).len()));
    }
}

fn on_start() {
    archive::extract("themes.zip", "themes", "themes", Default::default());
}
```

After:

```rust
import "lumen.cdl";

fn on_http(tag: string, response: any) {
    let r = response as {string: any};
    if r["ok"] as bool {
        lumen::signal_set("status", "got " + string((r["body"] as string).len()));
    }
}

fn on_start() {
    archive::extract("themes.zip", "themes", "themes", archive::ExtractOptions::default());
}
```

candela 0.1.6 has more changes, among them Option-returning `first`, `last`,
`min`, `max` and `find`, `throw "kind";` as a statement, renamed trim methods,
and `std/fs` as a module to import. The
[candela 0.1.6 migration guide](https://docs.lumenfx.dev/candela/migration/v0.1.6/)
lists every one.
