# Text draws the same on the GPU and the CPU renderer

This affects apps and tools that compare screenshots of Lumen text against
saved images, and Rust code that implements a `lumen_paint::Painter` sink.

Glyph outlines are now flattened by Lumen, once per glyph and size, and both
render backends fill the same polygons. Text edges therefore shift by a
fraction of a pixel's coverage compared with earlier releases, closer to the
true outline than before, and the GPU and CPU renderers draw them alike.
Re-capture any screenshot baselines that include text.

A custom `Painter` sink that should draw text the way the built-in backends do
fills outline runs through `lumen_paint::GlyphOutlines` from its
`draw_glyphs`, and calls `GlyphOutlines::end_frame` once a frame:

```rust
use lumen_paint::kurbo::Rect;
use lumen_paint::{GlyphOutlines, GlyphRun, Painter};

fn draw_glyphs(&mut self, run: &GlyphRun<'_>) {
    let mut outlines = std::mem::take(&mut self.outlines);
    let visible = Rect::new(0.0, 0.0, self.width, self.height);
    let drawn = outlines.draw(self, run, visible);
    self.outlines = outlines;
    if !drawn {
        // A colour glyph face (COLR or bitmaps): draw the run natively.
    }
}
```
