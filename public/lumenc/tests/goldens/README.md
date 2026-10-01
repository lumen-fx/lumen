# Golden screenshots

Checked-in baselines for the screenshot regression suite in
`public/lumenc/tests/golden.rs`. Each PNG is a 400x300, dpr-1 offscreen
capture of one small markup+CSS app driven through the full headless pipeline,
using the same plugin stack as `lumenc run` and no window.

Every case renders on each render backend the machine can run and compares
each frame against the same PNG: the CPU backend always, and the GPU backend
when wgpu finds a hardware adapter or Mesa's lavapipe. One baseline for both
is the check that an app looks the same whichever renderer draws it.

## Running

```sh
cargo test -p lumenc --test golden
```

The cases run with the rest of the suite, on CI too. A few are `#[ignore]`d,
each with its reason in the attribute; run those with `-- --ignored`.

Text is shaped with the font files in `fonts/` and nothing installed on the
machine, so every machine draws the same glyphs. `fonts/NotoSans-Regular.ttf`
is Noto Sans cut down to Latin text; `fonts/OFL.txt` is its license.

Each case captures twice per backend from two independent app builds and
fails as nondeterministic if the two frames disagree, before any golden
comparison happens.

## Updating baselines

```sh
LUMEN_GOLDEN_UPDATE=1 cargo test -p lumenc --test golden
```

rewrites every golden from the current build: from the GPU capture where
there is a GPU, else from the CPU one. The other backend is still compared
against what was written. Re-baseline after an intentional visual change
(skin edits, renderer fixes), look at the new images, and commit the PNG diffs
alongside the code change.

## Comparison model

Byte-equality is the wrong bar, because rasterization drifts across drivers,
vello versions, and the two backends. A pixel counts as different only when a
channel exceeds a small absolute delta, and a case fails only when the
fraction of differing pixels exceeds its threshold. Both constants, with their
rationale, live at the top of `tests/golden.rs`; the self-consistency pass
uses a tighter pair than the golden comparison.

On mismatch the test writes `actual.png` and `diff.png` (a heatmap where
yellow and red mark pixels past the delta) under
`$CARGO_TARGET_DIR/lumen-golden-failures/<case>/<backend>/` and prints the
paths.

## What the harness pins

A fixed 400x300 viewport at dpr 1, the fonts above, a forced dark color scheme
so the OS theme cannot leak in, `inertia="0"` on scrolled cases, settle
windows long enough to clamp every hover and press tween, and a caret that
never blinks because it paints whenever the entity is focused.
