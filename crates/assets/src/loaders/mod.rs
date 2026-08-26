//! The loaders the asset pipeline ships with.
//!
//! Each one is an ordinary [`crate::AssetLoader`] with no privileges over a
//! loader an app or plugin registers: [`AssetLoaders::default`] registers
//! these two and nothing else, and registering another loader for the same
//! extension replaces the built-in one.
//!
//! [`AssetLoaders::default`]: crate::AssetLoaders::default

mod image;
mod svg;

// `self::` qualified: the raster loader's module shares its name with the
// `image` crate it decodes through.
pub use self::image::{IMAGE_EXTENSIONS, ImageLoader};
pub use self::svg::{SVG_EXTENSIONS, SvgLoader};
