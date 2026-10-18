//! The runtime's entry: crash handling for every interactive launch.
//!
//! The preflight runs before the app is built, so a crash while the window
//! opens or the GPU starts is already caught. It reads `[crash]` from
//! `lumen.toml`; `enabled = false` leaves crash handling out, and a headless
//! run (tests, automation) never installs it.
//!
//! The install half, run while the app is built, keeps the report's GPU
//! line current and tells the helper when the app ends normally.

use bevy_ecs::prelude::*;
use lumen_capability::{CapabilityEnv, Preflight, Select};
use lumen_core::app::App;
use lumen_core::render_backend::RendererInfo;
use lumen_core::tick::TickStage;
use serde::Deserialize;

use crate::Config;

/// Every app carries crash handling.
pub const SELECT: Select = Select::Always;

/// `[crash]` in `lumen.toml`.
#[derive(Default, Deserialize)]
struct CrashSection {
    enabled: Option<bool>,
}

/// The one `[window]` key read here: the title names the app in the dialog.
#[derive(Default, Deserialize)]
struct WindowSection {
    title: Option<String>,
}

/// Whether `env` asks for crash handling.
fn wanted(env: &CapabilityEnv) -> bool {
    !env.headless && env.section::<CrashSection>("crash").enabled.unwrap_or(true)
}

/// Install crash handling before the app is built.
pub fn preflight(env: &CapabilityEnv) -> Preflight {
    if wanted(env) {
        let name = env
            .section::<WindowSection>("window")
            .title
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| env.app_id.clone());
        crate::install(Config::for_app(&env.app_id, &name));
    }
    Preflight::Continue
}

/// Wire the app to the crash handling the preflight installed.
pub fn install(app: &mut App, _env: &CapabilityEnv) {
    if crate::installed().is_none() {
        return;
    }
    app.world.insert_resource(EndsNormally);
    app.add_systems(TickStage::Systems, report_renderer);
}

/// Dropped with the app's world. Dropped outside a panic, the app is
/// ending normally, and the panics it survived are not crashes.
#[derive(Resource)]
struct EndsNormally;

impl Drop for EndsNormally {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            crate::report_clean_exit();
        }
    }
}

/// Pass the renderer's report on whenever it changes.
fn report_renderer(info: Option<Res<RendererInfo>>) {
    if let Some(info) = info
        && info.is_changed()
    {
        crate::set_gpu(describe(&info));
    }
}

/// `AMD Radeon RX 7600 (Vulkan, radv Mesa 24.2), render-gpu`.
fn describe(info: &RendererInfo) -> String {
    let detail: Vec<&str> = [info.api.as_str(), info.driver.as_str()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect();
    let device = match (info.device.is_empty(), detail.is_empty()) {
        (true, _) => "no device named".to_string(),
        (false, true) => info.device.clone(),
        (false, false) => format!("{} ({})", info.device, detail.join(", ")),
    };
    if info.backend.is_empty() {
        device
    } else {
        format!("{device}, {}", info.backend)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(config: &str) -> CapabilityEnv {
        CapabilityEnv::new(
            "/tmp/demo",
            toml::from_str(config).expect("valid toml"),
            String::new(),
            false,
        )
    }

    #[test]
    fn crash_handling_is_on_unless_turned_off_or_headless() {
        assert!(wanted(&env("")));
        assert!(wanted(&env("[crash]\nenabled = true\n")));
        assert!(!wanted(&env("[crash]\nenabled = false\n")));
        assert!(!wanted(&env("").headless(true)));
    }

    #[test]
    fn the_gpu_line_names_device_api_driver_and_backend() {
        let info = RendererInfo {
            backend: "render-gpu",
            device: "AMD Radeon RX 7600".into(),
            api: "Vulkan".into(),
            driver: "radv Mesa 24.2".into(),
            ..RendererInfo::default()
        };
        assert_eq!(
            describe(&info),
            "AMD Radeon RX 7600 (Vulkan, radv Mesa 24.2), render-gpu"
        );
        let cpu = RendererInfo {
            backend: "render-cpu",
            ..RendererInfo::default()
        };
        assert_eq!(describe(&cpu), "no device named, render-cpu");
    }
}
