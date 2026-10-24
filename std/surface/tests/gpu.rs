//! `<surface>` on the renderers, end to end: an in-process producer through
//! the loopback, the reference producer in C in another process through the
//! Unix transport, and the CPU renderer, which cannot show shared frames.
//!
//! The GPU cases skip themselves, saying why, on a machine with no GPU
//! adapter, like every GPU suite in the tree. The C producer is compiled with
//! the system C compiler against the protocol crate's static library and
//! Vulkan; its headers come from the system (`libvulkan-dev`), or from
//! `VULKAN_INCLUDE` when set.

use bevy_ecs::message::Messages;
use lumen_core::app::App;
use lumen_core::components::{
    Color, LumenAttributes, LumenClasses, LumenId, LumenTag, Transform, Visible,
};
use lumen_core::render_backend::DeviceKind;
use lumen_core::traits::Renderer;
use lumen_render_cpu::CpuRendererPlugin;
use lumen_render_wgpu::{WgpuRenderer, WgpuRendererPlugin};
use lumen_surface::{SurfaceElement, SurfaceEvent, SurfacePlugin, SurfaceStateKind, Surfaces};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long one step may take before a test gives up. A software rasterizer
/// on a loaded CI runner compiles each shader the first time it runs, which
/// can take seconds; a passing run takes a fraction of this.
const PATIENCE: Duration = Duration::from_secs(60);

const W: u32 = 128;
const H: u32 = 96;

/// Surfaces are registered process-wide by id, so these run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Why the GPU cases cannot run here, or `None` when they can. A software
/// adapter (lavapipe) runs them: the colours they check are clears, which
/// every rasterizer draws exactly.
///
/// With `LUMEN_SURFACE_REQUIRE_HARDWARE` set (the manually triggered
/// hardware job sets it), a machine with no adapter, or with only a software
/// one, fails instead of skipping: that job exists to share frames between
/// two processes on a hardware GPU.
fn no_gpu() -> Option<String> {
    watch_validation();
    let hardware = std::env::var_os("LUMEN_SURFACE_REQUIRE_HARDWARE").is_some();
    let why = match WgpuRenderer::new_offscreen(bevy_ecs::entity::Entity::PLACEHOLDER, 4, 4) {
        Ok(mut renderer) => {
            let info = renderer.info();
            if !hardware || info.device_kind != DeviceKind::Cpu {
                return None;
            }
            format!("only a software adapter: {}", info.device)
        }
        Err(e) => format!("no wgpu adapter: {e}"),
    };
    assert!(
        !hardware,
        "LUMEN_SURFACE_REQUIRE_HARDWARE is set, and {why}"
    );
    Some(why)
}

/// What the Vulkan validation layer reported, in either process.
static VALIDATION: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Collects the messages the validation layer hands wgpu, which wgpu logs
/// from its debug messenger. With the layer off (the default) nothing
/// arrives. Turn it on with `VK_INSTANCE_LAYERS=VK_LAYER_KHRONOS_validation`
/// (and `VK_LAYER_PATH` when it is not installed system-wide); the reference
/// producer inherits the same variables and prints its reports.
struct ValidationLog;

impl log::Log for ValidationLog {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Warn && metadata.target() == "wgpu_hal::vulkan::instance"
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = record.args().to_string();
        eprintln!("validation: {line}");
        if line.starts_with("VALIDATION") {
            VALIDATION
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(line);
        }
    }

    fn flush(&self) {}
}

fn watch_validation() {
    static LOGGER: ValidationLog = ValidationLog;
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Warn);
    }
}

/// Fail when the validation layer reported anything.
fn assert_validation_clean() {
    let seen = std::mem::take(&mut *VALIDATION.lock().unwrap_or_else(|e| e.into_inner()));
    assert!(
        seen.is_empty(),
        "the validation layer reported:\n{}",
        seen.join("\n")
    );
}

fn gpu_app() -> App {
    let mut app = App::new();
    app.add_plugin(WgpuRendererPlugin::new(W, H));
    app.add_plugin(SurfacePlugin::default());
    let mut vp = lumen_core::window::primary_viewport_mut(&mut app.world);
    vp.size = glam::Vec2::new(W as f32, H as f32);
    vp.clear = Color::rgb(0.0, 0.0, 0.0);
    app
}

fn spawn_surface(
    app: &mut App,
    id: &str,
    at: (f32, f32),
    size: (f32, f32),
) -> bevy_ecs::entity::Entity {
    app.world
        .spawn((
            LumenTag(lumen_surface::TAG.into()),
            LumenId(id.into()),
            Transform {
                absolute: glam::Vec2::new(at.0, at.1),
                size: glam::Vec2::new(size.0, size.1),
                baseline_y: None,
            },
            Visible(true),
        ))
        .id()
}

fn read_back(app: &App) -> Vec<u8> {
    app.render_world
        .get_non_send::<WgpuRenderer>()
        .expect("renderer")
        .read_rgba8(lumen_core::window::primary_entity(&app.world).expect("a primary window"))
        .expect("readback")
}

fn pixel(pixels: &[u8], x: u32, y: u32) -> [u8; 3] {
    let i = ((y * W + x) * 4) as usize;
    [pixels[i], pixels[i + 1], pixels[i + 2]]
}

fn near(got: [u8; 3], want: [u8; 3]) -> bool {
    got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= 4)
}

/// Tick until `check` holds, up to `limit`.
fn tick_until(app: &mut App, what: &str, limit: Duration, mut check: impl FnMut(&mut App) -> bool) {
    let deadline = Instant::now() + limit;
    loop {
        app.tick();
        if check(app) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn events(app: &mut App) -> Vec<SurfaceEvent> {
    app.world
        .resource_mut::<Messages<SurfaceEvent>>()
        .drain()
        .collect()
}

fn element(app: &mut App, e: bevy_ecs::entity::Entity) -> SurfaceElement {
    app.world.get::<SurfaceElement>(e).expect("adopted").clone()
}

/// A texture made in-process on the renderer's own device shows in the
/// element, contained in its box, with nothing copied.
#[test]
fn an_in_process_producer_shows_through_the_loopback() {
    let _serial = serial();
    if let Some(why) = no_gpu() {
        eprintln!("skipping: {why}");
        return;
    }
    let mut app = gpu_app();
    let e = spawn_surface(&mut app, "loop", (0.0, 0.0), (128.0, 64.0));
    app.tick();
    let mut producer = Surfaces
        .loopback("loop")
        .expect("the surface is registered");
    assert_eq!(producer.wanted_size(), (128, 64));
    let context = producer
        .context()
        .expect("the GPU renderer published its device");
    let texture = solid(&context, (64, 64), [0, 180, 0, 255]);
    producer.present(texture, b"m1").expect("presented");
    tick_until(&mut app, "the frame", PATIENCE, |app| {
        near(pixel(&read_back(app), 64, 32), [0, 180, 0])
    });
    let pixels = read_back(&app);
    // A square frame contained in a 128x64 box sits in the middle 64x64.
    assert!(
        near(pixel(&pixels, 40, 32), [0, 180, 0]),
        "inside the frame"
    );
    assert!(
        near(pixel(&pixels, 10, 32), [0, 0, 0]),
        "the letterbox shows the background"
    );
    assert!(
        near(pixel(&pixels, 64, 80), [0, 0, 0]),
        "outside the element"
    );
    assert_eq!(element(&mut app, e).state(), SurfaceStateKind::Live);
    let classes = app.world.get::<LumenClasses>(e).unwrap();
    assert!(classes.has("surface-live"));
    assert_eq!(
        app.world.get::<LumenAttributes>(e).unwrap().get("state"),
        Some("live")
    );
    assert!(
        events(&mut app)
            .iter()
            .any(|ev| matches!(ev, SurfaceEvent::Connected { .. }))
    );
    app.world.despawn(e);
    app.tick();
    assert!(
        Surfaces.state("loop").is_none(),
        "a removed element's surface goes"
    );
    assert_validation_clean();
}

/// A `size` texture on `context`'s device filled with one sRGB colour.
fn solid(
    context: &lumen_module::gpu::GpuContext,
    size: (u32, u32),
    rgba: [u8; 4],
) -> lumen_module::gpu::wgpu::Texture {
    use lumen_module::gpu::wgpu;
    let texture = context.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("test frame"),
        size: wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    context.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &rgba.repeat((size.0 * size.1) as usize),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size.0 * 4),
            rows_per_image: None,
        },
        wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        },
    );
    texture
}

/// Under the CPU renderer a surface cannot show shared frames: it says it is
/// unsupported, and its endpoint still answers so an app can start a producer
/// that will be refused with the reason.
#[test]
fn the_cpu_renderer_leaves_a_surface_unsupported() {
    let _serial = serial();
    let mut app = App::new();
    app.add_plugin(CpuRendererPlugin::new(W, H));
    app.add_plugin(SurfacePlugin::default());
    let e = spawn_surface(&mut app, "cpu", (0.0, 0.0), (64.0, 64.0));
    for _ in 0..3 {
        app.tick();
    }
    assert_eq!(element(&mut app, e).state(), SurfaceStateKind::Unsupported);
    assert!(
        app.world
            .get::<LumenClasses>(e)
            .unwrap()
            .has("surface-unsupported")
    );
    assert!(
        events(&mut app)
            .iter()
            .any(|ev| matches!(ev, SurfaceEvent::Error { reason, .. } if reason == "unsupported"))
    );
    app.world.despawn(e);
    app.tick();
}

/// The reference producer in C, through the Unix transport and Vulkan memory
/// export: the cross-process path this build ships on Linux.
#[cfg(target_os = "linux")]
mod cross_process {
    use super::*;
    use lumen_core::input::{
        Focused, Hovered, Key, KeyCode, KeyPressed, Modifiers, PointerData, PointerMoved,
    };
    use std::io::{BufRead, BufReader};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::sync::Arc;

    /// Build the reference producer, once per test process.
    fn reference_producer() -> PathBuf {
        static BUILT: Mutex<Option<PathBuf>> = Mutex::new(None);
        let mut built = BUILT.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(path) = built.as_ref() {
            return path.clone();
        }
        let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let source = crate_dir.join("protocol/producer/reference.c");
        let include = crate_dir.join("protocol/include");
        // The static library cargo built for the protocol crate sits beside this
        // test's own binary.
        let deps = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let library = deps.join("liblumen_surface_protocol.a");
        assert!(
            library.is_file(),
            "the protocol crate's static library is missing at {}",
            library.display()
        );
        let out = std::env::temp_dir().join(format!("lsp-reference-{}", std::process::id()));
        let mut cc = Command::new(std::env::var("CC").unwrap_or_else(|_| "cc".into()));
        cc.arg("-std=c11")
            .arg("-Wall")
            .arg("-Werror")
            .arg("-o")
            .arg(&out)
            .arg(&source)
            .arg("-I")
            .arg(&include);
        if let Ok(vulkan) = std::env::var("VULKAN_INCLUDE") {
            cc.arg("-I").arg(vulkan);
        }
        cc.arg(&library)
            .args(["-lvulkan", "-lpthread", "-ldl", "-lm", "-lgcc_s"]);
        let status = cc.status().expect("a C compiler (cc)");
        assert!(status.success(), "the reference producer does not compile");
        *built = Some(out.clone());
        out
    }

    /// A running reference producer and what it printed.
    struct Running {
        child: Child,
        log: Arc<Mutex<Vec<String>>>,
    }

    impl Running {
        fn start(endpoint: &str, env: &[(&str, &str)]) -> Self {
            let mut child = Command::new(reference_producer())
                .env("LUMEN_SURFACE", endpoint)
                .envs(env.iter().copied())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("the reference producer starts");
            let log = Arc::new(Mutex::new(Vec::new()));
            let stderr = child.stderr.take().unwrap();
            let sink = log.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    eprintln!("{line}");
                    sink.lock().unwrap().push(line);
                }
            });
            // The validation layer, when on, reports to standard output.
            let stdout = child.stdout.take().unwrap();
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    eprintln!("producer: {line}");
                    if line.contains("Validation Error") || line.contains("Validation Warning") {
                        VALIDATION
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push(format!("producer: {line}"));
                    }
                }
            });
            Self { child, log }
        }

        /// The inode of each buffer of generation `g`, as the producer saw its
        /// own export.
        fn inodes(&self, generation: u32) -> Vec<u64> {
            let prefix = format!("lsp-reference: buffer {generation}.");
            self.log
                .lock()
                .unwrap()
                .iter()
                .filter_map(|l| l.strip_prefix(&prefix))
                .filter_map(|rest| rest.split("inode ").nth(1)?.trim().parse().ok())
                .collect()
        }
    }

    impl Drop for Running {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// The reference producer in another process renders into the surface
    /// through the Unix transport: its buffers are the ones the consumer
    /// imported (the same inodes on both sides), input reaches it and comes back
    /// in its frames, a resize brings a new ring, and a crash leaves the last
    /// frame up until a new producer takes over.
    #[test]
    fn a_producer_in_another_process_shows_with_no_copy() {
        let _serial = serial();
        if let Some(why) = no_gpu() {
            eprintln!("skipping: {why}");
            return;
        }
        let mut app = gpu_app();
        let e = spawn_surface(&mut app, "game", (0.0, 0.0), (W as f32, H as f32));
        app.tick();
        let endpoint = Surfaces.endpoint("game").expect("registered");
        assert!(
            endpoint.contains(";adapter=vk:"),
            "the endpoint names the GPU: {endpoint}"
        );
        let first = Running::start(&endpoint, &[]);

        // The pattern: blue, with the orange square in the middle.
        tick_until(&mut app, "the first frame", PATIENCE, |app| {
            near(pixel(&read_back(app), 4, 4), [0, 0, 231])
        });
        let pixels = read_back(&app);
        assert!(
            near(pixel(&pixels, W / 2, H / 2), [255, 188, 0]),
            "the square: {:?}",
            pixel(&pixels, W / 2, H / 2)
        );
        let surface = element(&mut app, e);
        assert_eq!(surface.state(), SurfaceStateKind::Live);
        assert_eq!(surface.frame().unwrap().size, (W, H));

        // The consumer holds the producer's own buffers.
        let imported: Vec<u64> = surface
            .buffer_identities()
            .iter()
            .map(|(_, ino)| *ino)
            .collect();
        let exported = first.inodes(1);
        assert!(!exported.is_empty());
        assert_eq!(
            imported, exported,
            "the consumer imported the producer's buffers"
        );

        // A pointer move over the surface moves the square there.
        app.world.entity_mut(e).insert(Hovered);
        app.world.write_message(PointerMoved {
            position: glam::Vec2::new(24.0, 24.0),
            local: None,
            pointer: PointerData::mouse(0).stamped(),
        });
        tick_until(
            &mut app,
            "the square to follow the pointer",
            PATIENCE,
            |app| {
                let p = read_back(app);
                near(pixel(&p, 24, 24), [255, 188, 0]) && near(pixel(&p, W / 2, H / 2), [0, 0, 231])
            },
        );

        // A key on the focused surface swaps the background.
        app.world.entity_mut(e).insert(Focused);
        app.world.write_message(KeyPressed {
            key: Key::Character("g".into()),
            code: KeyCode::KeyG,
            modifiers: Modifiers::default(),
            repeat: false,
            time_stamp: lumen_core::time::Instant::now(),
        });
        tick_until(&mut app, "the key to reach the producer", PATIENCE, |app| {
            near(pixel(&read_back(app), 100, 80), [0, 231, 0])
        });

        // A smaller element brings a new ring at the new size. The resize
        // event goes out on the tick the size changes, and a message lives
        // two ticks, so they are gathered while the new ring comes.
        app.world.get_mut::<Transform>(e).unwrap().size = glam::Vec2::new(64.0, 48.0);
        let mut seen = Vec::new();
        tick_until(&mut app, "the resized frame", PATIENCE, |app| {
            seen.extend(events(app));
            element(app, e).frame().is_some_and(|f| f.size == (64, 48))
        });
        assert_eq!(element(&mut app, e).frame().unwrap().generation, 2);
        assert!(seen.iter().any(|ev| matches!(
            ev,
            SurfaceEvent::Resized {
                width: 64,
                height: 48,
                ..
            }
        )));
        drop(first);

        // The producer was killed: the surface is lost, says it was a crash,
        // and keeps its last frame. The session thread sets the state the
        // element reads at once, and the event goes out on the next tick, so
        // the wait gathers events until both are in.
        let mut seen = Vec::new();
        tick_until(&mut app, "the loss", PATIENCE, |app| {
            seen.extend(events(app));
            element(app, e).state() == SurfaceStateKind::Lost
                && seen.iter().any(
                    |ev| matches!(ev, SurfaceEvent::Disconnected { reason, .. } if reason == "crash"),
                )
        });
        assert!(
            near(pixel(&read_back(&app), 10, 10), [0, 231, 0]),
            "the last frame stays"
        );

        // A producer that dies on its own is a crash too; the next one takes
        // over the same surface.
        let mut crashing = Running::start(&endpoint, &[("LSP_REF_CRASH_AFTER", "3")]);
        let mut connected = false;
        tick_until(&mut app, "the crash", PATIENCE, |app| {
            let seen = events(app);
            connected |= seen
                .iter()
                .any(|ev| matches!(ev, SurfaceEvent::Connected { .. }));
            connected
                && seen.iter().any(
                    |ev| matches!(ev, SurfaceEvent::Disconnected { reason, .. } if reason == "crash"),
                )
        });
        let status = crashing.child.wait().expect("the crashing producer ends");
        assert_eq!(
            status.code(),
            Some(7),
            "it exited on its own, with no goodbye"
        );
        let again = Running::start(&endpoint, &[]);
        tick_until(&mut app, "a new producer", PATIENCE, |app| {
            element(app, e).state() == SurfaceStateKind::Live
                && near(pixel(&read_back(app), 4, 4), [0, 0, 231])
        });
        drop(again);
        app.world.despawn(e);
        app.tick();
        assert_validation_clean();
    }
}
