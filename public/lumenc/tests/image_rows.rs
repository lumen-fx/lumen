// Needs `RunOptions` / `build_headless_app`, only available under `dev-run`.
#![cfg(feature = "dev-run")]

//! An `<image>` in a `<for>` row, headless, from real sources: the file its
//! `src` placeholder names is the file it shows.
//!
//! The markup's relative paths are made absolute against the app directory
//! before the rows are built, and a `src` written as `{row.thumb}` is
//! relative as written, so a record holding an absolute path loaded
//! `<app dir>//...`.

use lumen_assets::{ImageSource, LoadedImage};
use lumen_core::components::LumenId;
use lumen_core::signals::{ArrayItem, ArraySignals};
use lumenc::RunOptions;
use lumenc::run::build_headless_app;
use std::path::{Path, PathBuf};

const MARKUP: &str = r#"<root id="app">
  <column>
    <for each="rows" key="id">
      <image id="pic-{row.id}" src="{row.thumb}" />
    </for>
  </column>
</root>
"#;

fn write_png(path: &Path, size: u32) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    image::RgbaImage::from_pixel(size, size, image::Rgba([255, 0, 0, 255]))
        .save(path)
        .expect("write png");
}

fn row(id: &str, thumb: &str) -> ArrayItem {
    [("id", id), ("thumb", thumb)]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// The source and decoded width of the image with `id`.
fn picture(app: &mut lumen_core::prelude::App, id: &str) -> (Option<PathBuf>, Option<u32>) {
    let mut q = app
        .world
        .query::<(&LumenId, Option<&ImageSource>, Option<&LoadedImage>)>();
    q.iter(&app.world)
        .find(|(lid, _, _)| lid.0.as_str() == id)
        .map(|(_, src, img)| (src.map(|s| s.0.clone()), img.map(|i| i.width)))
        .unwrap_or_default()
}

/// Tick until the image with `id` has decoded a picture `width` wide.
fn shows(
    app: &mut lumen_core::prelude::App,
    id: &str,
    width: u32,
) -> (Option<PathBuf>, Option<u32>) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        app.tick();
        let now = picture(app, id);
        if now.1 == Some(width) || std::time::Instant::now() > deadline {
            return now;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// A temporary app directory holding `MARKUP` and the pictures `pngs`
/// names (path under the base, width), with nothing left from an earlier run.
fn app_dir(name: &str, pngs: &[(&str, u32)]) -> (PathBuf, PathBuf) {
    let base = std::path::absolute(
        std::env::temp_dir().join(format!("lumen_image_rows_{name}_{}", std::process::id())),
    )
    .unwrap();
    let _ = std::fs::remove_dir_all(&base);
    let dir = base.join("app");
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("main.lmn"), MARKUP).unwrap();
    // Port 0 keeps parallel test binaries off a shared socket.
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").unwrap();
    for (path, width) in pngs {
        write_png(&base.join(path), *width);
    }
    (base, dir)
}

/// The app directory relative paths resolve against is process-wide, so
/// the apps here run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn set_rows(app: &mut lumen_core::prelude::App, rows: Vec<ArrayItem>) {
    app.world.resource_mut::<ArraySignals>().set("rows", rows);
}

/// A relative path in a record loads from the app directory, and an
/// absolute one, from a cache directory outside it, loads as written.
#[test]
fn a_row_image_loads_the_file_its_record_names() {
    let _serial = serial();
    let (base, dir) = app_dir(
        "paths",
        &[("app/art/inside.png", 4), ("cache/thumb.png", 6)],
    );
    let thumb = base.join("cache/thumb.png");
    let opts = RunOptions::new(&dir).with_parser(lumenc::default_parser());
    let (mut app, _window) = build_headless_app(opts).expect("build_headless_app");
    set_rows(
        &mut app,
        vec![
            row("a", "art/inside.png"),
            row("b", thumb.to_str().unwrap()),
        ],
    );
    let inside = shows(&mut app, "pic-a", 4);
    let outside = shows(&mut app, "pic-b", 6);
    let _ = std::fs::remove_dir_all(&base);

    assert_eq!(
        inside,
        (Some(dir.join("art/inside.png")), Some(4)),
        "a relative path loads from the app"
    );
    assert_eq!(
        outside,
        (Some(thumb), Some(6)),
        "an absolute path loads as written"
    );
}
