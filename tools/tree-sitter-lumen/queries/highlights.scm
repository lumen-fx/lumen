; Lumen markup highlighting.
;
; Capture names follow the Neovim / Helix vocabulary. Zed ships its own copy
; of this file under tools/zed-lumen, because Zed reads queries from the
; extension rather than from the grammar repository and its theme keys differ.
;
; Patterns that could match the same node are partitioned with predicates
; instead of relying on query order, since editors disagree about whether the
; first or the last matching pattern wins.

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
] @tag.delimiter

; Reactive attributes: bind-text, bind-value, bind-checked, on-click, ...
((attribute_name) @attribute.builtin
  (#match? @attribute.builtin "^(bind|on)-"))

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
((signal_reference) @variable.builtin
  (#eq? @variable.builtin "$index"))

((signal_reference) @variable
  (#not-eq? @variable "$index"))

(processing_instruction) @preproc

(comment) @comment

(text) @none
