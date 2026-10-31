# `<menu>` keeps its items as elements

A `<menu>` outside a `<menubar>` used to become an `overlay` with the class
`menu-panel`, holding one `button.menu-item` per `<menuitem>` and a
`spacer.menu-separator` per `<separator>`, mounted the first time the menu
opened. It now stays a `menu` element holding `menuitem` and `separator`
elements, mounted with the page and shown while the menu is open. The
in-window menu bar's menus and a dock's tab and overflow menus are built the
same way. This affects you if a stylesheet or a script reached into that
structure:

- A rule for `.menu-panel`, `.menu-item` or `.menu-separator` styles nothing
  now; write it for `menu`, `menuitem` or `separator`. `.menu-accel` is
  unchanged.
- A menu item is no longer a `button`, so an app rule written for `button`
  stops reaching it; add `menuitem` to the selector to keep it. The built-in
  skins do this themselves, and a menu draws as it did.
- A script that waited for a menu to open before looking up its items finds
  them from the first frame now.

Rust code that built `lumen_ir::layout_ir::Attributes` with `menu_item` set
builds a `menuitem` element instead (`lumen_ir::menu::menu_item`); the
spawner wires its click to the menu it sits in.
