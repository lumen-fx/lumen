//! The `layout-taffy` capability: this engine in the app's layout-engine
//! registry, under the name `taffy`.

use lumen_capability::CapabilityEnv;
use lumen_core::app::App;
use lumen_core::backends::register_backend;
use lumen_core::layout_backend::LayoutBackend;
use lumen_core::traits::LayoutEngine;

use crate::TaffyLayoutPlugin;

/// The name the engine is registered under.
pub const NAME: &str = "taffy";

/// The rank among layout engines; the highest one is installed.
pub const PRIORITY: i32 = 0;

/// Register the engine. What the capability crate beside this one
/// registers.
pub fn install(app: &mut App, _env: &CapabilityEnv) {
    register_backend(
        app,
        LayoutBackend {
            name: NAME,
            priority: PRIORITY,
            engine,
        },
    );
}

fn engine() -> Box<dyn LayoutEngine> {
    Box::new(TaffyLayoutPlugin)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_core::components::{Length, Style, Transform};
    use lumen_core::layout_backend::install_layout;

    /// Installing the capability registers `taffy`, and the engine a launch
    /// installs from the registry lays a styled box out on the next tick.
    #[test]
    fn the_capability_registers_a_working_layout_engine() {
        let mut app = App::new();
        install(
            &mut app,
            &CapabilityEnv::new(".", toml::Table::new(), String::new(), false),
        );
        assert_eq!(install_layout(&mut app), Ok(NAME));

        let style = Style {
            width: Length::Px(120.0),
            height: Length::Px(40.0),
            ..Default::default()
        };
        let node = app.world.spawn((style, Transform::default())).id();
        app.tick();
        let t = app.world.get::<Transform>(node).expect("laid out");
        assert_eq!(t.size, glam::Vec2::new(120.0, 40.0));
    }
}
