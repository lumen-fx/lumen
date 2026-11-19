# candela scripts follow candela 0.2.0: `sort` needs an order, `break` needs a loop

This affects every app with candela scripts, in `.cdl` files or inline
`<script>` blocks. Lumen now links candela 0.2.0, and a script that relied on
one of these fails to compile:

| Before | After |
| --- | --- |
| `xs.sort()` on a list of structs, enums or bools, which did nothing | give the type an order with `fn <(self, other: T) -> bool`, or sort with `xs.sort_by(fn(a, b) => ...)` |
| `break` or `continue` in a function called from a loop | return a value and let the loop break |
| `ns::ns::Entity` for a struct another file's `host "ns"` block declares | `ns::Entity` |

An `any[]` that sorts has to hold all ints, all floats or all strings; a mix
raises `sort_without_order` when it sorts.

A `dylib "./native/mylib"` path in a script no longer resolves against the
directory the app was started from. Keep native libraries in the app's
`lib/`, where a bare `dylib "mylib"` finds them.

candela 0.2.0 also adds tuples. The
[candela 0.2.0 release](https://github.com/lumen-fx/candela/releases/tag/v0.2.0)
lists every change.
