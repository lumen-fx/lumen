# FFI guide

> **Status (updated).** The C-ABI surface has since **shipped** and is at
> **ABI 0.7** (`lumen_ffi::LUMEN_ABI_{MAJOR,MINOR,PATCH} = 0.7.0`, mirrored by
> `LUMEN_API_VERSION` in `lumen/ffi/include/lumen.h`). The lifecycle
> (`lumen_app_new`, `lumen_app_run`, `lumen_app_run_headless`, `lumen_app_free`),
> the exposed-callback surface, signal mutators, navigation, and
> `lumen_abi_version` are live; cbindgen generates `lumen_simple.h` and the
> C++/Python SDKs load `liblumen_ffi`. The prose below this banner predates that
> and is retained only for the design rationale.
>
> **ABI 0.7 (link-not-embed).** Added `lumen_app_new_from_lmna(data, len,
> base_dir)`: build an app from prebuilt LMNA artifact bytes with NO parser, so a
> thin launcher can compile source in-process and hand the bytes across the ABI
> to a dlopen'd liblumen instead of static-linking the runtime. See
> [`docs/design/link-not-embed.md`](../../design/link-not-embed.md).

## Today

```rust
// lumen/ffi/src/lib.rs

/// C-ABI status code returned by every `lumen_*` function.
#[repr(u32)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum LumenStatus {
    Ok = 0,
    InternalError = 1,
}
```

That's the entire shipped surface. The status enum is the convention
every function will use; no `lumen_*` entry points exist yet. There's
no `cbindgen` invocation wired into the build, no generated `lumen.h`,
and no `examples/c-client/` consumer (planned but blocked on the
surface itself landing).

## Invariants for the C-ABI

When the surface ships, these are the contracts every `lumen_*` fn
must obey. They're called out here so plugin authors writing FFI-style
bridges follow the same shape:

1. **No Rust panic may escape.** Every `lumen_*` fn wraps its body in
   `std::panic::catch_unwind` (or equivalent) and returns
   `LumenStatus::InternalError` on catch. Panics across the FFI
   boundary are undefined behaviour in Rust.

2. **Errors return `LumenStatus`.** No exceptions, no out-of-band
   error channels. Output values flow through `*mut`-out parameters
   when the call also has a return code.

3. **Structs are `#[repr(C)]`, not `#[repr(C, packed)]`.** Packed
   structs interact badly with C compilers across alignment-strict
   targets (per the FFI-soundness plan).

4. **Strings are `*const c_char` UTF-8.** Lumen does not transit
   wide-char / UTF-16 — the C-ABI matches Rust's internal UTF-8
   `&str` model. NUL terminator is required.

5. **Ownership is explicit.** Every allocator-returning function is
   paired with a `lumen_*_free`. Callers own released memory.

## Planned surface

The plan calls out four core operations every binding needs:

```c
/* Entity creation — returns an opaque handle. */
LumenStatus lumen_entity_create(LumenApp *app, LumenEntity *out);

/* Component / style mutation — flat key/value setters keyed on
   property name. Mirrors apply_attribute / apply_declaration in
   lumenc/src/parser_{html,css}.rs. */
LumenStatus lumen_style_set(
    LumenApp *app,
    LumenEntity e,
    const char *property,
    const char *value
);

/* Author-side command queueing — same shape as ScriptCommand from
   the Rhai host, just in C-callable form. */
LumenStatus lumen_queue_command(
    LumenApp *app,
    const LumenCommand *cmd
);

/* Dirty mask — bitfield reflecting which subsystems need redraw.
   Used by hosts that drive their own tick loop and want to skip work
   when nothing changed. Matches FrameDirty roll-up semantics. */
LumenStatus lumen_dirty_mask(
    const LumenApp *app,
    uint32_t *out_mask
);
```

The dirty mask bits will mirror the per-stage dirty flags already
maintained internally (layout, a11y, render). Bindings get the same
skip-work optimization the in-process tick loop has.

## cbindgen workflow

Once the Rust signatures are stable, `cbindgen` produces a
`lumen.h` header from them — no manual header maintenance.

```bash
# (planned)
cargo install cbindgen
cbindgen --crate lumen-ffi --output target/lumen.h
```

The header lands in `target/` and gets vendored into each downstream
binding crate (Python, Node, Go) so consumers don't depend on having
cbindgen installed.

## Per-language bindings

Each binding wraps the raw FFI in idiomatic ergonomics. The plan:

| Language | Crate / library | Pattern |
|---|---|---|
| C | `examples/c-client/` | Direct FFI consumer; reference driver. |
| Python | `lumen-py` (PyO3) | Python classes wrapping `LumenApp` / `LumenEntity` handles. `__del__` calls `lumen_*_free`. |
| Node / TypeScript | `lumen-node` (napi-rs) | Same pattern, async wrappers for any blocking calls. |
| Go | `lumen-go` (cgo) | Thin Go interfaces over C; channels for the dirty mask poll. |
| Lua | `lumen-script-lua` via `mlua` | Implements the in-process `ScriptHost` trait (parallel to `lumen-script-rhai`) — not the FFI per se. |

Bindings ride on top of the C-ABI; they don't depend on each other and
they don't depend on Cargo (a downstream Python project just needs
the `.so` / `.dylib` / `.dll` plus the header).

## When can I use this?

Today: not yet. The surface is committed to in the SDD and the
roadmap, but the FFI work comes after the multi-window story and is
gated on the in-process API stabilizing.

If you need to drive Lumen from another language today, the practical
options are:

1. **Spawn `lumenc` as a subprocess.** Author your UI in `.lmn` +
   `.rhai`, drive the script over the (planned) MCP introspection
   port (`[mcp] port = 7878`). The MCP surface is more surface-stable
   than the in-process API.
2. **Embed `lumen` as a Rust dependency.** If the host language has
   a Rust embedding story (uniffi, neon, etc.), use that as the
   bridge until the C-ABI ships.
3. **Wait.** The FFI work is on the active roadmap; the lift is
   well-scoped (the in-process API exposes everything the C-ABI needs
   — there's no architecture work, just careful API design).

## Why not finish FFI first?

Two reasons baked into the plan:

1. **Surface stability matters more than reach.** A C-ABI is harder
   to evolve than a Rust API; we want the in-process surface to
   settle (which means the widget set, the CSS subset, and the
   render pipeline all in their stable shape) before encoding it
   into a fixed-vocabulary ABI.

2. **The hard problems live in-process.** Multi-window, FFI safety,
   and per-language bindings are downstream of having an
   in-process model that's worth exposing. Premature ABI is a
   reliability tax we'd pay forever.

When the surface ships, this page will be replaced with the actual
function table, struct layouts, and worked C / Python / Node samples.

## Tracking

- Design: § 6 "Multi-language story" in
  [`docs/SDD.md`](https://github.com/lumen-ui/lumen/blob/main/docs/SDD.md).
- Code: `lumen/ffi/src/lib.rs` (stub).
