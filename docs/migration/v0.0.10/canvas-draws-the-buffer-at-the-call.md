# A canvas draws a pixel buffer as it was at the draw call

This affects scripts and apps that draw a pixel buffer onto a `<canvas>` and
then write to the buffer before the frame is drawn.

`canvas::draw_buffer` and `canvas::draw_buffer_scaled` now take the buffer's
pixels at the call, as `drawImage` does on the web. Before, a draw read the
buffer when the tick replayed the drawing, so a write later in the same
handler or tick showed up in a draw made before it. Now it shows only once the
buffer is drawn again, on every host, in a Rust app, and in a page.

A script that drew first and wrote after, expecting the write on screen,
moves the draw after the write:

```
// before: the fill reached the screen through the draw above it
canvas::draw_buffer("art", buf, 0.0, 0.0);
canvas::buffer_fill_rect(buf, 0, 0, 8, 8, 0xff0000ff);

// after
canvas::buffer_fill_rect(buf, 0, 0, 8, 8, 0xff0000ff);
canvas::draw_buffer("art", buf, 0.0, 0.0);
```

A draw of a buffer handle that names no buffer now records nothing at the
call, rather than being skipped when the drawing is replayed; what appears on
screen is the same.
