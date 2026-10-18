# `@font-face` loads its font on the desktop too

A `@font-face` rule used to reach only the web target; a desktop run ignored it
and drew with the fonts installed on the machine. The desktop now loads the
rule's font file from the app directory, so text naming its family draws with
it in `lumenc run` and in a packaged app as it does in a browser.

An app whose stylesheet already carries `@font-face` for its site changes look
on the desktop when the rule lists a TrueType or OpenType file. A rule that
lists only `woff` or `woff2` files is left to browsers and the desktop keeps
the system font; add a `ttf` or `otf` beside them to use the font on both
targets.
