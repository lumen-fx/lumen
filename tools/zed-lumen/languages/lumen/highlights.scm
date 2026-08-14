; Lumen markup highlighting for Zed.
;
; Same rules as tools/tree-sitter-lumen/queries/highlights.scm, with the
; capture names Zed themes define: tag delimiters are punctuation.bracket, and
; reactive attributes read as keywords because Zed has no attribute.builtin.
;
; Patterns that could match the same node are partitioned with predicates
; instead of relying on query order.

; Control flow and composition. These tags are not layout nodes: `<for>` and
; `<if>` drive reactive iteration and branching, and `<template>`, `<use>`,
; `<slot>`, and `<include>` are resolved before parsing.
((tag_name) @keyword
  (#any-of? @keyword "for" "if" "template" "use" "slot" "include"))

((tag_name) @tag
  (#not-any-of? @tag "for" "if" "template" "use" "slot" "include"))

[
  "<"
  ">"
  "</"
  "/>"
] @punctuation.bracket

; Reactive attributes: bind-text, bind-value, bind-checked, on-click, ...
((attribute_name) @keyword
  (#match? @keyword "^(bind|on)-"))

((attribute_name) @attribute
  (#not-match? @attribute "^(bind|on)-"))

"=" @operator

[
  "\""
  "'"
] @string

(attribute_value) @string

(entity) @string.escape

; `{name}`, `{$name}`, `{$self.field}`, `{$parent.field}`, `{row.field}`.
(interpolation
  [
    "{"
    "}"
  ] @punctuation.special)

(identifier) @variable

(row_reference) @variable

; `{$index}` is the iteration row index rather than a signal name.
((signal_reference) @constant.builtin
  (#eq? @constant.builtin "$index"))

((signal_reference) @variable
  (#not-eq? @variable "$index"))

(processing_instruction) @preproc

(comment) @comment
