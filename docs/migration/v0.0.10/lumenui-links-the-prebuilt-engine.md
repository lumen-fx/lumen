# The published `lumenui` links the release's prebuilt engine

This affects Rust SDK apps. `lumenui` no longer compiles the engine with your
app: its build script downloads the engine's compiled crates from the Lumen
release matching its version, and points the compiler at them. Three things
change for an app.

Pin the toolchain. The compiled crates are readable only by the rustc that
built them, so add a `rust-toolchain.toml` beside `Cargo.toml` with the channel
the release names. A build with another compiler stops and prints the file to
write:

```toml
[toolchain]
channel = "<the version the build names>"
```

Drop direct dependencies on the engine's crates. A dependency of your own on
`bevy_ecs`, `glam`, or any `lumen-*` crate now compiles a second copy whose
types do not match the engine's. Take them from `lumenui` instead:

```rust
// before: bevy_ecs = "0.19" in Cargo.toml, and
use bevy_ecs::prelude::Component;
// after: no extra dependency
use lumenui::prelude::*; // Component, Resource, Message, Bundle, Widget
let v = lumenui::glam::Vec2::ZERO;
```

Import the prelude with a glob (`use lumenui::prelude::*`): the `bevy_ecs`
derives find their crate through the `bevy_ecs` name it brings in.

Build on the machine you target: cross-compiling a `lumenui` app is refused.

In a Lumen checkout the SDK's code moved to the `lumenui-engine` crate in
`sdk/rust` (library `lumenui_engine`); `sdk/lumenui` is the crate apps depend
on. Something that depended on the checkout's `sdk/rust` by path keeps its
`lumenui::` paths with
`lumenui = { package = "lumenui-engine", path = ".../sdk/rust" }`, and
`cargo run -p lumenui --example counter` is now `-p lumenui-engine`.
