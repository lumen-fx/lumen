//! Every function in the `canvas` namespace, called.
//!
//! The bodies are what a script reaches, and they answer without a world: a
//! call records against the process-wide store and returns. So they can be
//! driven straight off the registry the plugin fills, one call at a time,
//! with no app, no window, and no host in the way. What that buys over the
//! host-level suite is the arms a working app never takes - a colour that is
//! not a colour, a cap that has been exceeded, a handle nothing answers for.

use lumen_canvas::CanvasPlugin;
use lumen_canvas::store::{self, Caps};
use lumen_core::app::App;
use lumen_script::{ScriptFn, ScriptFnRegistry, ScriptNs, ScriptValue};

/// The store is process-global, so these run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The `canvas` surface, as the registry holds it after the plugin built.
fn surface() -> Vec<ScriptFn> {
    let mut app = App::new();
    app.add_plugin(CanvasPlugin::default());
    app.world
        .resource::<ScriptFnRegistry>()
        .fns()
        .iter()
        .filter(|f| matches!(&f.ns, ScriptNs::Named(ns) if ns == "canvas"))
        .cloned()
        .collect()
}

/// Call one function by name, or say it does not exist.
fn call(fns: &[ScriptFn], name: &str, args: &[ScriptValue]) -> ScriptValue {
    let f = fns
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("the canvas namespace has no `{name}`"));
    let (result, _commands) = f.invoke(args);
    result.unwrap_or_else(|e| panic!("{name} failed: {e:?}"))
}

fn s(v: &str) -> ScriptValue {
    ScriptValue::Str(v.to_string())
}

fn f(v: f64) -> ScriptValue {
    ScriptValue::F64(v)
}

fn i(v: i64) -> ScriptValue {
    ScriptValue::I64(v)
}

fn as_int(v: ScriptValue) -> i64 {
    match v {
        ScriptValue::I64(n) => n,
        other => panic!("expected an integer, got {other:?}"),
    }
}

fn as_bool(v: ScriptValue) -> bool {
    match v {
        ScriptValue::Bool(b) => b,
        other => panic!("expected a boolean, got {other:?}"),
    }
}

/// Every drawing call, once. What this asserts is that each one is bound,
/// takes the arguments its documentation says, and records: the calls land in
/// the surface's journal, in order, and none of them raises.
#[test]
fn every_drawing_call_is_bound_and_records() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    store::reset();
    let fns = surface();
    let id = || s("chart");

    let calls: Vec<(&str, Vec<ScriptValue>)> = vec![
        ("begin_path", vec![id()]),
        ("move_to", vec![id(), f(0.0), f(0.0)]),
        ("line_to", vec![id(), f(10.0), f(0.0)]),
        ("quad_to", vec![id(), f(12.0), f(2.0), f(10.0), f(10.0)]),
        (
            "bezier_to",
            vec![id(), f(9.0), f(12.0), f(2.0), f(12.0), f(0.0), f(10.0)],
        ),
        (
            "arc",
            vec![
                id(),
                f(5.0),
                f(5.0),
                f(4.0),
                f(0.0),
                f(std::f64::consts::FRAC_PI_2),
            ],
        ),
        ("rect", vec![id(), f(0.0), f(0.0), f(4.0), f(4.0)]),
        ("close_path", vec![id()]),
        ("fill", vec![id()]),
        ("stroke", vec![id()]),
        ("fill_rect", vec![id(), f(0.0), f(0.0), f(8.0), f(8.0)]),
        ("stroke_rect", vec![id(), f(0.0), f(0.0), f(8.0), f(8.0)]),
        ("set_fill_rgba", vec![id(), f(1.0), f(0.0), f(0.0), f(1.0)]),
        (
            "set_stroke_rgba",
            vec![id(), f(0.0), f(0.0), f(1.0), f(1.0)],
        ),
        ("set_line_width", vec![id(), f(2.0)]),
        ("set_global_alpha", vec![id(), f(0.5)]),
        ("save", vec![id()]),
        ("restore", vec![id()]),
        ("translate", vec![id(), f(1.0), f(1.0)]),
        ("rotate", vec![id(), f(0.5)]),
        ("scale", vec![id(), f(2.0), f(2.0)]),
        ("reset_transform", vec![id()]),
        (
            "set_transform",
            vec![id(), f(1.0), f(0.0), f(0.0), f(1.0), f(5.0), f(5.0)],
        ),
        ("fill_text", vec![id(), s("hello"), f(0.0), f(12.0)]),
        ("clear", vec![id()]),
    ];
    let expected = calls.len();
    for (name, args) in &calls {
        assert_eq!(call(&fns, name, args), ScriptValue::Unit, "{name}");
    }

    let mut store = store::store();
    assert_eq!(
        store.surface("chart").pending.len(),
        expected,
        "every call recorded exactly one operation"
    );
}

/// The calls that answer a value rather than recording one.
#[test]
fn the_canvas_answers_its_own_size() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    store::reset();
    let fns = surface();

    // A canvas nothing has touched reports the size the HTML canvas has
    // always defaulted to.
    assert_eq!(as_int(call(&fns, "width", &[s("chart")])), 300);
    assert_eq!(as_int(call(&fns, "height", &[s("chart")])), 150);

    call(&fns, "resize", &[s("chart"), f(64.0), f(32.0)]);
    assert_eq!(as_int(call(&fns, "width", &[s("chart")])), 64);
    assert_eq!(as_int(call(&fns, "height", &[s("chart")])), 32);

    // A negative size is nonsense rather than an error; it floors at nothing.
    call(&fns, "resize", &[s("chart"), f(-5.0), f(-5.0)]);
    assert_eq!(as_int(call(&fns, "width", &[s("chart")])), 0);
}

/// The style setters answer whether they understood what they were given, so
/// a script can branch instead of catching.
#[test]
fn a_style_that_is_not_understood_is_refused_rather_than_guessed_at() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    store::reset();
    let fns = surface();

    for (name, good, bad) in [
        ("set_fill_style", "#ff8800", "chartreuse"),
        ("set_stroke_style", "rgba(0,0,0,0.5)", "hsl(200,50%,50%)"),
        ("set_line_cap", "round", "flat"),
        ("set_line_join", "bevel", "sharp"),
        ("set_font", "bold 16px Inter", "Inter"),
    ] {
        assert!(
            as_bool(call(&fns, name, &[s("chart"), s(good)])),
            "{name} understood '{good}'"
        );
        assert!(
            !as_bool(call(&fns, name, &[s("chart"), s(bad)])),
            "{name} accepted '{bad}'"
        );
    }

    // Only the five understood values recorded anything.
    assert_eq!(store::store().surface("chart").pending.len(), 5);
}

/// The whole buffer surface, including what it does with a handle nothing
/// answers for.
#[test]
fn the_buffer_surface_round_trips_and_refuses_safely() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    store::reset();
    let fns = surface();

    let handle = as_int(call(&fns, "buffer_new", &[i(4), i(4)]));
    assert!(handle > 0);
    assert_eq!(as_int(call(&fns, "buffer_width", &[i(handle)])), 4);
    assert_eq!(as_int(call(&fns, "buffer_height", &[i(handle)])), 4);

    call(
        &fns,
        "buffer_set_pixel",
        &[i(handle), i(1), i(1), i(0xff00_00ff)],
    );
    assert_eq!(
        as_int(call(&fns, "buffer_get_pixel", &[i(handle), i(1), i(1)])),
        0xff00_00ff
    );

    call(
        &fns,
        "buffer_fill_rect",
        &[i(handle), i(0), i(0), i(2), i(2), i(0x00ff_00ff)],
    );
    assert_eq!(
        as_int(call(&fns, "buffer_get_pixel", &[i(handle), i(0), i(0)])),
        0x00ff_00ff
    );

    let region = call(
        &fns,
        "buffer_get_region",
        &[i(handle), i(0), i(0), i(2), i(2)],
    );
    let ScriptValue::Array(items) = &region else {
        panic!("a region is an array, got {region:?}");
    };
    assert_eq!(items.len(), 4);

    call(
        &fns,
        "buffer_put_region",
        &[
            i(handle),
            i(2),
            i(2),
            i(2),
            i(1),
            ScriptValue::Array(vec![i(0x0000_ffff), i(0x0000_ffff)]),
        ],
    );
    assert_eq!(
        as_int(call(&fns, "buffer_get_pixel", &[i(handle), i(2), i(2)])),
        0x0000_ffff
    );

    // Drawing one onto a canvas, both ways.
    call(
        &fns,
        "draw_buffer",
        &[s("chart"), i(handle), f(0.0), f(0.0)],
    );
    call(
        &fns,
        "draw_buffer_scaled",
        &[s("chart"), i(handle), f(0.0), f(0.0), f(16.0), f(16.0)],
    );

    assert!(as_bool(call(&fns, "buffer_free", &[i(handle)])));
    assert!(
        !as_bool(call(&fns, "buffer_free", &[i(handle)])),
        "freeing twice is refused rather than freeing someone else's buffer"
    );

    // Every read of a handle nothing answers for is a value, not a raise.
    assert_eq!(as_int(call(&fns, "buffer_width", &[i(handle)])), 0);
    assert_eq!(as_int(call(&fns, "buffer_height", &[i(9999)])), 0);
    assert_eq!(
        as_int(call(&fns, "buffer_get_pixel", &[i(9999), i(0), i(0)])),
        0
    );
    call(&fns, "buffer_set_pixel", &[i(9999), i(0), i(0), i(1)]);
    // A handle nothing answers for reads as no buffer at all, so the region
    // comes back empty rather than as a rectangle of transparent pixels: the
    // script can tell "outside this buffer" from "there is no buffer".
    let empty = call(
        &fns,
        "buffer_get_region",
        &[i(9999), i(0), i(0), i(1), i(1)],
    );
    assert_eq!(empty, ScriptValue::Array(Vec::new()));
    // A negative handle is not a handle at all.
    assert_eq!(as_int(call(&fns, "buffer_width", &[i(-1)])), 0);
}

/// A region takes whatever numbers the host handed it.
///
/// Every host spells numbers differently and coerces on the way in, so the
/// array a script passes can hold floats where the signature says integers.
/// A pixel that is no number at all is transparent rather than a raise.
#[test]
fn a_region_takes_the_numbers_a_host_actually_passes() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    store::reset();
    let fns = surface();

    let handle = as_int(call(&fns, "buffer_new", &[i(2), i(2)]));
    call(
        &fns,
        "buffer_put_region",
        &[
            i(handle),
            i(0),
            i(0),
            i(2),
            i(2),
            ScriptValue::Array(vec![
                f(255.0),
                ScriptValue::Bool(true),
                s("511"),
                ScriptValue::Unit,
            ]),
        ],
    );
    assert_eq!(
        as_int(call(&fns, "buffer_get_pixel", &[i(handle), i(0), i(0)])),
        255
    );
    assert_eq!(
        as_int(call(&fns, "buffer_get_pixel", &[i(handle), i(1), i(0)])),
        1
    );
    assert_eq!(
        as_int(call(&fns, "buffer_get_pixel", &[i(handle), i(0), i(1)])),
        511
    );
    assert_eq!(
        as_int(call(&fns, "buffer_get_pixel", &[i(handle), i(1), i(1)])),
        0,
        "a value that is no number is transparent"
    );

    // Something that is not a list at all writes nothing rather than raising.
    call(
        &fns,
        "buffer_put_region",
        &[i(handle), i(0), i(0), i(1), i(1), s("not a list")],
    );
    assert_eq!(
        as_int(call(&fns, "buffer_get_pixel", &[i(handle), i(0), i(0)])),
        255
    );
}

/// Asking a canvas its size while other calls are pending.
#[test]
fn the_size_survives_calls_that_are_not_resizes() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    store::reset();
    let fns = surface();

    call(
        &fns,
        "fill_rect",
        &[s("chart"), f(0.0), f(0.0), f(4.0), f(4.0)],
    );
    assert_eq!(
        as_int(call(&fns, "width", &[s("chart")])),
        300,
        "a drawing call says nothing about the drawing space"
    );

    // The last resize wins, even with drawing recorded after it.
    call(&fns, "resize", &[s("chart"), f(10.0), f(10.0)]);
    call(&fns, "resize", &[s("chart"), f(20.0), f(20.0)]);
    call(
        &fns,
        "fill_rect",
        &[s("chart"), f(0.0), f(0.0), f(4.0), f(4.0)],
    );
    assert_eq!(as_int(call(&fns, "width", &[s("chart")])), 20);
}

/// The caps, from the script's side: a call over one answers empty and says
/// why, rather than allocating what it was asked for.
#[test]
fn a_call_over_a_cap_answers_empty() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    store::reset();
    let fns = surface();
    store::store().caps = Caps {
        region: 1024,
        buffer_pixels: 4096,
        buffer_count: 2,
    };

    // A buffer larger than the per-buffer cap.
    assert_eq!(as_int(call(&fns, "buffer_new", &[i(128), i(128)])), 0);
    // A buffer with no pixels in it.
    assert_eq!(as_int(call(&fns, "buffer_new", &[i(0), i(4)])), 0);
    // A negative size is not a size.
    assert_eq!(as_int(call(&fns, "buffer_new", &[i(-4), i(4)])), 0);

    let first = as_int(call(&fns, "buffer_new", &[i(4), i(4)]));
    let second = as_int(call(&fns, "buffer_new", &[i(4), i(4)]));
    assert!(first > 0 && second > 0);
    assert_eq!(
        as_int(call(&fns, "buffer_new", &[i(4), i(4)])),
        0,
        "the third is over the count cap"
    );

    // A region larger than the region cap answers empty and writes nothing.
    let over = call(
        &fns,
        "buffer_get_region",
        &[i(first), i(0), i(0), i(64), i(64)],
    );
    assert_eq!(over, ScriptValue::Array(Vec::new()));
    call(
        &fns,
        "buffer_put_region",
        &[
            i(first),
            i(0),
            i(0),
            i(64),
            i(64),
            ScriptValue::Array(vec![i(1)]),
        ],
    );
    assert_eq!(
        as_int(call(&fns, "buffer_get_pixel", &[i(first), i(0), i(0)])),
        0,
        "the refused write left the buffer alone"
    );

    // A fill is bounded by the buffer rather than by the region cap, so a
    // buffer the app allowed can always be filled whole.
    call(
        &fns,
        "buffer_fill_rect",
        &[i(first), i(0), i(0), i(64), i(64), i(0x00ff_00ff)],
    );
    assert_eq!(
        as_int(call(&fns, "buffer_get_pixel", &[i(first), i(3), i(3)])),
        0x00ff_00ff
    );

    store::store().caps = Caps::default();
}

/// PNG, both directions, and what a path that is not one answers.
#[test]
fn a_png_round_trips_through_the_script_surface() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    store::reset();
    let fns = surface();

    let dir = std::env::temp_dir().join(format!("lumen-canvas-surface-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    lumen_core::app_paths::set_app(dir.clone(), "lumen-canvas-surface".to_string());
    let path = "round-trip.png";

    let handle = as_int(call(&fns, "buffer_new", &[i(3), i(2)]));
    call(
        &fns,
        "buffer_set_pixel",
        &[i(handle), i(0), i(0), i(0xff00_0080)],
    );
    assert!(as_bool(call(
        &fns,
        "buffer_save_png",
        &[i(handle), s(path)]
    )));

    let loaded = as_int(call(&fns, "buffer_load_png", &[s(path)]));
    assert!(loaded > 0 && loaded != handle);
    assert_eq!(as_int(call(&fns, "buffer_width", &[i(loaded)])), 3);
    assert_eq!(
        as_int(call(&fns, "buffer_get_pixel", &[i(loaded), i(0), i(0)])),
        0xff00_0080,
        "straight alpha survives the round trip"
    );

    // A path with nothing at it, and a handle with no buffer.
    assert_eq!(as_int(call(&fns, "buffer_load_png", &[s("absent.png")])), 0);
    assert!(!as_bool(call(&fns, "buffer_save_png", &[i(9999), s(path)])));
    // A path that cannot be written.
    assert!(!as_bool(call(
        &fns,
        "buffer_save_png",
        &[i(handle), s("no-such-dir/out.png")]
    )));

    let _ = std::fs::remove_dir_all(&dir);
}

/// The caps a module config asks for are clamped, and a config that says
/// nothing leaves the defaults.
///
/// Through the module entry rather than around it: `install_with` is what the
/// loader calls, so this is the same path an app's `config` table takes.
#[test]
fn the_configured_caps_reach_the_surface() {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    let install = |config: &str| {
        store::reset();
        let mut app = App::new();
        let status = lumen_module::install_with(&mut app, config, CanvasPlugin::new);
        assert_eq!(status, lumen_module::INSTALL_OK, "installing `{config}`");
        store::store().caps
    };

    let caps = install("region_cap = 2048\nbuffer_pixel_cap = 8192\nbuffer_count_cap = 3\n");
    assert_eq!(
        (caps.region, caps.buffer_pixels, caps.buffer_count),
        (2048, 8192, 3)
    );

    // Out of range in both directions, and a count that is not a count.
    let caps = install("region_cap = 999999999999\nbuffer_pixel_cap = 1\nbuffer_count_cap = -4\n");
    assert_eq!(caps.region, lumen_canvas::MAX_REGION_CAP);
    assert_eq!(caps.buffer_pixels, lumen_canvas::MIN_BUFFER_PIXEL_CAP);
    assert_eq!(caps.buffer_count, lumen_canvas::MIN_BUFFER_COUNT_CAP);

    // A key of the wrong type says nothing about the cap, so the default
    // stands rather than the module refusing to install.
    let caps = install("region_cap = \"lots\"\n");
    assert_eq!(caps.region, lumen_canvas::DEFAULT_REGION_CAP);

    let caps = install("");
    assert_eq!(caps.region, lumen_canvas::DEFAULT_REGION_CAP);
    assert_eq!(caps.buffer_pixels, lumen_canvas::DEFAULT_BUFFER_PIXEL_CAP);
    assert_eq!(caps.buffer_count, lumen_canvas::DEFAULT_BUFFER_COUNT_CAP);

    store::reset();
}
