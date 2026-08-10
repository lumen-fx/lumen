# Golden screenshots

Checked-in baselines for the screenshot regression suite in
`lumenc/tests/golden.rs`. Each PNG is a 400x300, dpr-1 offscreen wgpu+vello
capture of one small markup+CSS app driven through the full headless pipeline,
using the same plugin stack as `lumenc run` and no window.

## Running

```sh
cargo test -p lumenc --test golden
```

Cases skip themselves, with a printed reason instead of a failure, when:

- there is no usable GPU adapter, or only a software one;
- `CI` is set in the environment, because a runner resolves a different
  default sans-serif than the machine that captured the baselines.

Each case captures twice from two independent app builds and fails as
nondeterministic if the two frames disagree, before any golden comparison
happens.

## Updating baselines

```sh
LUMEN_GOLDEN_UPDATE=1 cargo test -p lumenc --test golden
```

rewrites every golden from the current build. Re-baseline after any
intentional visual change (skin edits, renderer fixes) and commit the
PNG diffs alongside the code change.

## Comparison model

Byte-equality is the wrong bar, because GPU rasterization drifts across
drivers and vello versions. A pixel counts as different only when a channel
exceeds a small absolute delta, and a case fails only when the fraction of
differing pixels exceeds its threshold. Both constants, with their rationale,
live at the top of `tests/golden.rs`; the self-consistency pass uses a tighter
pair than the golden comparison.

On mismatch the test writes `actual.png` and `diff.png` (a heatmap where
yellow and red mark pixels past the delta) under
`$CARGO_TARGET_DIR/lumen-golden-failures/<case>/` and prints the paths.

## Portability caveats

- Text is shaped with the machine's system fonts through cosmic-text, so the
  baselines are machine-local. Comparing against baselines produced on a box
  with different fonts or a different GPU driver generation may need a
  re-baseline. Treat these as regression tripwires for one machine, not
  cross-platform truth.
- The harness pins everything else: a fixed 400x300 viewport at dpr 1, a
  forced dark color scheme so the OS theme cannot leak in, `inertia="0"` on
  scrolled cases, settle windows long enough to clamp every hover and press
  tween, and a caret that never blinks because it paints whenever the entity
  is focused.
