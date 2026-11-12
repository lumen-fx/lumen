# The in-window menu bar's shortcuts leave a focused text field's keys alone

This affects apps whose `<menubar>` is drawn in the window (Linux, or
`[window] menubar = "window"`). An item's `accel` used to fire whatever had
focus, so Ctrl+Z in a text field both undid the typing and ran the menu's
Undo. Now a focused text field keeps typing, Shift with a key, and its editing
chords (Ctrl or Command with A, C, X, V, Z or Y, the arrows, Home, End,
Backspace, Delete), and a focused element that takes every key (a
`<surface keyboard="all">`, or one holding the pointer lock) keeps them all.
A chord a command from the `lumen-commands` module answers goes to the
command rather than the bar. Every other chord still reaches the bar. An app that relied on Ctrl+Z reaching
its Undo item from inside a field handles the field's `keydown` instead.
