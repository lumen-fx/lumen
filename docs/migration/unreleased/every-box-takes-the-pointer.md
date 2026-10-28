# Clicks and hover land on unpainted elements, and `:active` holds for ancestors

Every element with a box now takes the pointer, as in a browser. Before, only
an element with a background or border, a scroll container, a tab stop, or a
declared `cursor` could be under the pointer, so a click on a plain row, or on
the label inside a button, landed on the nearest painted ancestor instead.
Now the innermost element under the pointer is the target, painted or not.

This affects code that reads the target of a pointer event and expects the
interactive element itself:

- A DOM handler's `target` may be an inner element, such as a button's label
  or a row's text. Read `current_target`, the element the handler is bound
  on, or walk up from `target` to the element you mean.
- `on_click(id)`, `on_double_click(id)` and `on_long_press(id)` receive the
  id of the nearest element at or above the target that has one, so a click
  on an id-less label inside `<button id="save">` still arrives as `"save"`.
  A label with an id of its own now reports that id.
- Rust code that reads `ClickEvent::entity`, `Hovered`, or `Pressed` sees
  the innermost element. Walk `ChildOf` up to the component you look for.
- An empty layout box drawn over other content, such as a full-window
  overlay that only positions a panel, now hides that content from the
  pointer. Give it `pointer-events: none`, and `pointer-events: auto` to
  the part that should take the pointer.

`:active` and the press tint now hold for the pressed element and its
ancestors, as `:hover` already did, so a button stays `:active` while the
press is on its label. A release clicks the nearest element holding both
ends of the press: pressing a button's label and releasing over the button's
padding clicks the button. Rust code asking whether an element is pressed in
the `:active` sense reads the new `PressWithin` marker; `Pressed` stays on
the element the press went down on.

```css
/* before: the overlay took no pointer because it painted nothing */
.overlay { width: 100%; height: 100%; }
/* after: let the pointer through to the app, and take it on the panel */
.overlay { width: 100%; height: 100%; pointer-events: none; }
.panel { pointer-events: auto; }
```

```rust
// before: the click named the button, since its label was never hit
fn on_click(clicks: MessageReader<ClickEvent>, buttons: Query<&MyButton>) {}
// after: walk up from the target to the button
fn on_click(
    mut clicks: MessageReader<ClickEvent>,
    buttons: Query<&MyButton>,
    parents: Query<&ChildOf>,
) {
    for click in clicks.read() {
        let button = std::iter::once(click.entity)
            .chain(parents.iter_ancestors(click.entity))
            .find_map(|e| buttons.get(e).ok());
    }
}
```
