# Golden screenshots

Checked-in baselines for the screenshot regression suite in
`lumenc/tests/golden.rs`. Each PNG is a 400x300, dpr-1 offscreen
wgpu+vello capture of one small markup+CSS app driven through the full
headless pipeline (same plugin stack as `lumenc run`, no window).

## Running

```sh
cargo test -p lumenc --test golden
```

- No GPU adapter (bare CI container): every case skips with a message.
- Each case captures **twice** from two independent app builds and
  fails as *nondeterministic* if the two frames disagree, before any
  golden comparison happens.

## Updating baselines

```sh
LUMEN_GOLDEN_UPDATE=1 cargo test -p lumenc --test golden
```

rewrites every golden from the current build. Re-baseline after any
intentional visual change (skin edits, renderer fixes) and commit the
PNG diffs alongside the code change.

## Comparison model

Not byte-equality - GPU rasterization drifts slightly across drivers:

- per-pixel, per-channel delta <= 4/255 is ignored;
- at most 0.1 % of pixels may exceed that.

Constants (with rationale) live at the top of `tests/golden.rs`. On
mismatch the test writes `actual.png` + `diff.png` (heatmap: yellow/red
= exceeding delta) under `$CARGO_TARGET_DIR/lumen-golden-failures/<case>/`
and prints the paths.

## Portability caveats

- Text is shaped with the machine's system fonts (cosmic-text). Goldens
  are therefore **machine-local**: comparing against baselines produced
  on a box with different fonts or a different GPU driver generation may
  need a re-baseline. Treat these as regression tripwires for one
  machine/CI image, not cross-platform truth.
- The harness pins everything else: fixed 400x300 viewport, dpr 1,
  forced dark color scheme (OS theme cannot leak in), `inertia="0"` on
  scrolled cases, wall-clock settle windows that clamp all hover/press
  tweens, and a caret that never blinks (painted whenever focused).
