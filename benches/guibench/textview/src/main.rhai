// textview bench app. guibench fills the empty doc-col in main.lmn with
// 5,000 wrapped paragraphs from its shared corpus before a run; Lumen's
// Rhai API has no file I/O, so the corpus ships inside the markup.
// Nothing reactive to set up; scrolling is driven externally by the
// harness over MCP wheel events, like the list app.

fn on_start() {}
