# A Rust system that looks elements up enters its app's DOM

This affects Rust SDK code that runs more than one app in a process, most
often a test binary whose tests each build an app, and that calls
`dom::get_by_id`, `dom::query` or `Node` traversal from its own system.

Each app now keeps its own element index. A lookup reads the app whose tick
is running on the calling thread; a system can run on a worker thread shared
by every app, so it states its app by taking `Res<AppDom>` and holding
`dom.enter()`:

```rust
// before
fn mark_done(mut clicks: MessageReader<ClickEvent>) {
    if clicks.read().count() > 0 && let Some(label) = dom::get_by_id("status") {
        label.set_text("done");
    }
}

// after
fn mark_done(mut clicks: MessageReader<ClickEvent>, dom: Res<AppDom>) {
    let _dom = dom.enter();
    if clicks.read().count() > 0 && let Some(label) = dom::get_by_id("status") {
        label.set_text("done");
    }
}
```

A system that does not enter reads the tree of the process's only app, so a
process with one app needs no change. With several apps alive it reads an
empty tree, where it used to read whichever app had ticked last. `AppDom` is
in the prelude.
