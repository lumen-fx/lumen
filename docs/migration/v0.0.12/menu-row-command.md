# Menu rows and menu bar items carry a command

This affects Rust code that builds a `lumen_ir::menu::MenuRow::Item` or a
`lumen_ir::layout_ir::MenuEntrySpec::Item` with a struct literal, or calls
`lumen_ir::menu::menu_item`. Each gains `command: Option<String>` (a fifth
`command` argument for `menu_item`), the command the item runs; give it
`None`. In markup, a `<menuitem>` with a `command` no longer needs an `id`.
