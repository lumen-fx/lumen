# `<tabs>` keeps its tabs as elements

A `<tabs>` used to become a `column` with the class `tabs`, holding a strip
and one hidden `<if>` per tab. It now stays a `tabs` element holding the same
`.tab-strip` followed by its `<tab>` elements, which are the panels. This
affects you if a stylesheet or a script reached into that structure:

- A rule for `.tabs` styles nothing now; write it for `tabs`.
- A selector that walked from the strip to the bodies through `if` elements
  finds `tab` elements instead, and a script that looked for an `<if>`
  between a tab set and its content finds the content directly under the
  `<tab>`.
- A `<tab>` written with `disabled` now disables its content as well as its
  button.
- A `<tabs>` and its `<tab>` are two levels of the tree, so a document within
  a level of the 32-level nesting cap per tab set now goes over it and is
  refused as too deeply nested.

The strip, its buttons, their classes and the `bind-value` signal are as
they were. `bind-value` is now optional.

Rust code that matched `TabStripButton { signal_name, value }` reads the
tab from `TabStripButton { tab }` and the tab set's signal from
`Tabs { bind, .. }`; `sync_tab_selected` is `sync_tabs`.
