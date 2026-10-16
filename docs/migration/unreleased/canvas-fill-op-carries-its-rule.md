# A recorded canvas fill carries its fill rule

This affects Rust code that builds or matches the canvas module's recorded
calls, `lumen_canvas::ops::Op`, directly. Scripts, and Rust apps that draw
through `Context2d`, are not affected.

`Op::Fill` is now `Op::Fill(FillRule)`, because a fill can be nonzero or
even-odd (`Context2d::fill_evenodd`, `canvas::fill_evenodd`). Write
`Op::Fill(FillRule::NonZero)` where `Op::Fill` was, and match `Op::Fill(_)`.
`FillRule` is exported from `lumen_canvas` and `lumenui_canvas`.
