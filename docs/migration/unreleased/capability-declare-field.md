# `Capability` gains a `declare` field

This affects code that builds a `lumen_capability::Capability` by hand rather
than through `lumen_capability!`.

The struct has a new field, `declare: Option<fn(&mut App)>`, which registers
the script functions a capability offers so a compile can declare them. A
capability written with the macro needs no change; add `declare = <fn>` to
the macro call when scripts call functions the capability registers. A struct
literal gains the field:

```rust
// before
Capability { name, phase, install, preflight: None, select, crate_name }
// after
Capability { name, phase, install, preflight: None, declare: None, select, crate_name }
```
