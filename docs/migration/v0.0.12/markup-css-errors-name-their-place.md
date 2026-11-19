# Markup and CSS errors name their place

This affects Rust code that reads the errors of the markup and CSS parser,
or implements `SourceParser`.

- `lumenc::ParseError` gains `At { offset, error }`: an error placed at a
  byte of the parsed text. Most parse errors now come wrapped in it, so a
  `match` on `BadAttribute`, `Xml` or another variant matches
  `err.unplaced()` instead. `err.offset()` is the byte, when the error names
  one. An `UnknownTag` message no longer says its position; its offset is
  where it always was.
- `RunError::ParseHtml` and `RunError::ParseCss` carry a
  `lumen_ir::source_map::SourceError` in place of a `String`: the message,
  and the file, line and column it is at when known. Its `Display` reads
  `file:line:col: message`.
- `SourceParser::parse_html`, `parse_css`, `parse_html_with_loader`,
  `collect_fragments` and `script_refs` return a `SourceError`, and
  `resolve_includes` and `resolve_css_imports` return the spliced text with
  a `SourceMap` of the file each line came from.
