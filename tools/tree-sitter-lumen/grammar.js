/**
 * Tree-sitter grammar for Lumen markup (`.lmn`).
 *
 * Lumen markup is an XML-shaped subset of HTML: every element closes, every
 * attribute value is quoted, and the document is parsed by `roxmltree` inside
 * `lumenc`. The grammar mirrors that shape rather than HTML's, so there is no
 * implicit tag closing and no unquoted attribute value, and none of it needs
 * an external scanner.
 *
 * On top of XML, Lumen adds `{interpolation}` placeholders in text and in
 * attribute values, with the `$signal`, `$self.field`, `$parent.field`,
 * `$index`, and `row.field` reference forms.
 *
 * `<script>` bodies are raw text: script source cannot contain `<` or a bare
 * `&` and stay valid XML, so the body token runs to the next `<`.
 */

/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

module.exports = grammar({
  name: 'lumen',

  extras: ($) => [/\s+/, $.comment],

  rules: {
    document: ($) => repeat($._node),

    _node: ($) =>
      choice(
        $.element,
        $.processing_instruction,
        $.interpolation,
        $.entity,
        $.text,
      ),

    element: ($) =>
      choice(
        seq($.start_tag, repeat($._node), $.end_tag),
        $.self_closing_tag,
        $.script_element,
      ),

    start_tag: ($) => seq('<', $.tag_name, repeat($.attribute), '>'),

    self_closing_tag: ($) => seq('<', $.tag_name, repeat($.attribute), '/>'),

    end_tag: ($) => seq('</', $.tag_name, '>'),

    // `<script>` collects script source instead of a layout node, so its body
    // is not markup. Aliasing the tags back to the ordinary tag node types
    // keeps queries written against `start_tag` / `end_tag` working.
    script_element: ($) =>
      choice(
        seq(
          alias($.script_start_tag, $.start_tag),
          optional($.raw_text),
          alias($.script_end_tag, $.end_tag),
        ),
        alias($.script_self_closing_tag, $.self_closing_tag),
      ),

    script_start_tag: ($) =>
      seq('<', alias($._script_tag_name, $.tag_name), repeat($.attribute), '>'),

    script_self_closing_tag: ($) =>
      seq(
        '<',
        alias($._script_tag_name, $.tag_name),
        repeat($.attribute),
        '/>',
      ),

    script_end_tag: ($) =>
      seq('</', alias($._script_tag_name, $.tag_name), '>'),

    // Wins over `tag_name` on an exact match; `<scripted>` still lexes as an
    // ordinary tag name because the longer match takes priority.
    _script_tag_name: (_) => token(prec(1, 'script')),

    raw_text: (_) => token(prec(-1, /[^<]+/)),

    tag_name: (_) => /[A-Za-z_][A-Za-z0-9_.:-]*/,

    attribute: ($) =>
      seq($.attribute_name, optional(seq('=', $.quoted_attribute_value))),

    attribute_name: (_) => /[^<>"'=/\s]+/,

    quoted_attribute_value: ($) =>
      choice(
        seq(
          '"',
          repeat(
            choice(
              alias($._attribute_text_double, $.attribute_value),
              $.interpolation,
              $.entity,
            ),
          ),
          '"',
        ),
        seq(
          "'",
          repeat(
            choice(
              alias($._attribute_text_single, $.attribute_value),
              $.interpolation,
              $.entity,
            ),
          ),
          "'",
        ),
      ),

    // Precedence over the whitespace extra so a run of spaces inside a value
    // stays part of the value.
    _attribute_text_double: (_) => token(prec(1, /[^"{}&]+/)),
    _attribute_text_single: (_) => token(prec(1, /[^'{}&]+/)),

    // `{name}`, `{$name}`, `{$self.field}`, `{$parent.field}`, `{$index}`,
    // and `{row.field}`. Anything else between braces is carried as opaque
    // text so an unrecognized placeholder does not fail the parse.
    interpolation: ($) =>
      seq(
        '{',
        optional(
          choice(
            seq(
              choice($.signal_reference, $.row_reference, $.identifier),
              optional($.interpolation_text),
            ),
            $.interpolation_text,
          ),
        ),
        '}',
      ),

    signal_reference: (_) => token(prec(1, /\$[A-Za-z_][A-Za-z0-9_.-]*/)),

    row_reference: (_) => token(prec(1, /row\.[A-Za-z_][A-Za-z0-9_.-]*/)),

    identifier: (_) => /[A-Za-z_][A-Za-z0-9_.-]*/,

    interpolation_text: (_) => token(prec(-1, /[^{}\n]+/)),

    entity: (_) => /&(#[0-9]+|#[xX][0-9a-fA-F]+|[A-Za-z][A-Za-z0-9._-]*);/,

    text: (_) => /[^<{}&\s]([^<{}&]*[^<{}&\s])?/,

    processing_instruction: (_) => token(seq('<?', /[^>]*/, '>')),

    comment: (_) =>
      token(seq('<!--', repeat(choice(/[^-]/, /-[^-]/, /--[^>]/)), '-->')),
  },
});
