# Dropdown lists and menus open in the top layer, and a dialog's body is there from the start

The floating list of a `<dropdown>` and the panel of a `<menu>` are now
popovers. While one is open it is laid out against the window rather than
inside the element it is written in, so a scrolling side panel or an
`overflow: hidden` box no longer clips it, and every part of it takes
clicks. It stays inside the window: it flips above its trigger near the
bottom edge, lines up on the trigger's right edge near the right edge, and
shrinks to the room it has. A `<menu>` opened with `open_menu(id)` and no
point opens below where it is written.

This affects you if a stylesheet placed these panels itself:

- `inset`, `position` and `width` written for `.dropdown-panel` or `menu`
  no longer move an open panel, because the placement holds them while it is
  open. Use `position-area`, `position-try-fallbacks` and `popup-gap`
  instead (see the CSS reference, Popovers).
- A dropdown's list takes its header's width while open.

A `<dialog>`, a popover, a `<menu>` and a dropdown's list now run their
entering `transition` (an `opacity` fade, say) every time they open, not
only the first time.

A `<dialog>`'s body is now mounted when the page is, hidden until the dialog
opens, instead of the first time it opens. A script that looked for an
element inside a dialog before it ever opened finds it now, and a handler
that opens a dialog can style what is inside it on the same tick. An
`on_ready` or startup code that assumed those elements did not exist yet
sees them.

`open="..."` on an element that is neither a `<dialog>` nor a `popover`,
and `anchor="..."` on one that is not a `popover`, are now errors; both used
to be ignored.

For Rust plugins: `lumen_primitives::flip_open_dropdown_panels` and
`place_menus_at_points` are gone. `Popover` and `place_popovers` place every
popover, and `PopupPanel` now only marks the built panels whose rows the
arrow keys walk.
