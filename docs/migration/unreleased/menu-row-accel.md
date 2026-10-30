# Menu rows carry a shortcut

This affects Rust code that builds a `lumen_ir::menu::MenuRow::Item` with a
struct literal, such as a module that opens a popup menu of its own. The
variant gains `accel: Option<String>`, the keyboard shortcut the row shows
beside its label (`"CommandOrControl+Z"`). In a literal, give it `accel: None`
for a row with no shortcut.
