; Language injection for `<script>` bodies.
;
; A `<script src="...">` names its host through the file extension, so the
; editor opens that file as .cdl, .rhai, or .lua and needs no injection here.
; An inline body has no such marker: which host runs it is decided by the
; app's other script files and by `[script] engine` in lumen.toml, neither of
; which a grammar can see. Rhai is the default this grammar assumes, matching
; the VS Code grammar and every inline body in the example apps but one.
;
; Keep Lua and candela sources in `<script src="...">` files. candela has no
; upstream tree-sitter grammar, so there would be nothing to inject for it in
; any case.
;
; An editor without a rhai parser installed shows the body as plain text.
((script_element
  (raw_text) @injection.content)
  (#set! injection.language "rhai"))
