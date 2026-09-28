# `process::start` takes a fourth argument, `opts`

This affects scripts in apps that declare `lumen-process`. `start` now takes
an options value after the tag: the directory the child starts in (`cwd`),
variables to add to its environment (`env`), and whether to end it when the
app exits (`end_at_exit`). The argument is required, so a three-argument call
no longer compiles in candela and raises in Rhai and Lua.

Pass the defaults to keep the previous behaviour: the child starts in the app
directory, inherits the environment, and outlives the app.

candela, before and after:

```rust
process::start("git", ["status"], "git");
process::start("git", ["status"], "git", Default::default());
```

Rhai:

```rhai
process::start("git", ["status"], "git");
process::start("git", ["status"], "git", #{});
```

Lua:

```lua
process.start("git", {"status"}, "git")
process.start("git", {"status"}, "git", {})
```

To set a field, name it and take the rest from the defaults, for example
`process::StartOptions { cwd: "instances/a", ..Default::default() }` in
candela. The options are listed with `process::start` in each host's scripting
reference. `process::stop(tag)` is new and ends a child you started.
