//! What a build rendered per request leaves a server to render from.
//!
//! A site built with `render = "ssr"` writes no documents; a server produces
//! them. Everything the build knew about the site that the compiled app does
//! not carry travels in one file beside it, [`SERVER_SPEC_FILE`]: where the
//! files are, what the pages are called, which locales the site answers in
//! and which catalogues they read, the size of every image, the declared
//! state a render starts from, and the policy the app set for what a render
//! may reach. A server reads that file and the files it names, and holds
//! everything the build held.
//!
//! The browser never reads it. What a page loads is the manifest, and this
//! file changes nothing about it; it has a version of its own,
//! [`SERVER_SPEC_VERSION`], which moves when its shape does.

use std::fmt;

use lumen_html::PixelSize;
use lumen_html::contract::Seed;
use serde::{Deserialize, Serialize};

use crate::spec::{AssetRef, PageSpec, WebSpec};

/// The file a build rendered per request writes at the site root.
pub const SERVER_SPEC_FILE: &str = "lumen.site.json";

/// The shape of [`ServerSpec`] this build reads and writes.
///
/// A server built against another version refuses the file rather than
/// guessing at it. Any change to what the file holds, a field added,
/// removed, renamed or read differently, moves this: the file refuses a
/// field it does not know, and a missing one, so two shapes under one
/// number cannot read each other. The golden file in this crate's tests
/// fails when the shape moves without it.
///
/// 2: the site settings name the browser add-ons the documents load and the
/// elements they answer for.
pub const SERVER_SPEC_VERSION: u32 = 2;

/// Everything a server needs to render a built site, beyond the compiled app.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSpec {
    /// The shape this file is written in. See [`SERVER_SPEC_VERSION`].
    pub version: u32,
    /// The site-wide settings the build emitted with: the base path, the
    /// address and canonical URL, the social image, how the pages are styled
    /// and navigated, the entry page, and the hashed names of the artifact,
    /// the stylesheet, the runtime pair and each catalogue.
    pub web: WebSpec,
    /// Every locale the site answers in, the one at the site root first.
    pub locales: Vec<String>,
    /// The chain a key missing from the active catalogue falls through, as
    /// `[app] fallback_locale` names it. Empty takes the default chain.
    pub fallback: Vec<String>,
    /// What each page says about itself in its `<head>`.
    pub pages: Vec<PageHead>,
    /// The size of every image the site carries whose header gave one, which
    /// a document writes as `<img width height>`.
    pub images: Vec<ImageSize>,
    /// The state every render starts from: the `[web.seed]` values and the
    /// defaults the markup declares.
    pub seed: Seed,
    /// What the app allows a render to reach.
    pub policy: ServerPolicy,
}

/// What one page says about itself in its `<head>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageHead {
    /// The page key.
    pub key: String,
    /// The page's own title. `None` takes the site title.
    pub title: Option<String>,
    /// The page's own description. `None` takes the site description.
    pub description: Option<String>,
    /// Whether a crawler is invited to index the page.
    pub index: bool,
}

impl PageHead {
    /// What `page` says about itself.
    pub fn of(page: &PageSpec) -> Self {
        Self {
            key: page.key.clone(),
            title: page.title.clone(),
            description: page.description.clone(),
            index: page.index,
        }
    }

    /// Write this head onto `page`.
    pub fn apply(&self, page: &mut PageSpec) {
        page.title = self.title.clone();
        page.description = self.description.clone();
        page.index = self.index;
    }
}

/// An image the site carries, by its path from the site root, and its size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageSize {
    /// Where the image is, relative to the site root.
    pub path: String,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// What the app allows a render to reach, from `lumen.toml` `[web.ssr]`.
///
/// This is the app's half of the policy. Where the server listens, how many
/// workers it runs and anything else about the machine is the server's.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerPolicy {
    /// The hosts a render may ask for data, by name.
    pub allow_hosts: Vec<String>,
    /// How many requests one render may make. `None` takes the renderer's
    /// default.
    pub max_requests: Option<usize>,
    /// Request headers the app may read beyond the ones every render allows,
    /// such as `authorization` or `cookie`.
    pub headers: Vec<String>,
}

/// A spec file that cannot be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerSpecError {
    /// The file was written in a shape this build does not read.
    Version {
        /// The version the file says it is.
        found: u32,
        /// The version this build reads.
        expected: u32,
    },
    /// The file is not a spec file.
    Decode(String),
}

impl fmt::Display for ServerSpecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServerSpecError::Version { found, expected } => write!(
                f,
                "{SERVER_SPEC_FILE} is version {found}, and this build reads version {expected}; \
                 build the site again with the lumenc that matches this server"
            ),
            ServerSpecError::Decode(why) => write!(f, "{SERVER_SPEC_FILE}: {why}"),
        }
    }
}

impl std::error::Error for ServerSpecError {}

/// The one field every version of the file has.
#[derive(Deserialize)]
struct Versioned {
    version: u32,
}

impl ServerSpec {
    /// A spec for `web`, with nothing else in it yet.
    pub fn new(web: WebSpec) -> Self {
        Self {
            version: SERVER_SPEC_VERSION,
            web,
            locales: Vec::new(),
            fallback: Vec::new(),
            pages: Vec::new(),
            images: Vec::new(),
            seed: Seed::new(),
            policy: ServerPolicy::default(),
        }
    }

    /// Read a spec file's bytes.
    ///
    /// # Errors
    ///
    /// The bytes are not a spec file, or one written in another version.
    pub fn from_json(bytes: &[u8]) -> Result<Self, ServerSpecError> {
        let versioned: Versioned = serde_json::from_slice(bytes)
            .map_err(|error| ServerSpecError::Decode(error.to_string()))?;
        if versioned.version != SERVER_SPEC_VERSION {
            return Err(ServerSpecError::Version {
                found: versioned.version,
                expected: SERVER_SPEC_VERSION,
            });
        }
        serde_json::from_slice(bytes).map_err(|error| ServerSpecError::Decode(error.to_string()))
    }

    /// The file's contents. Every map in it is ordered, so the same site
    /// writes the same bytes.
    pub fn to_json(&self) -> String {
        let mut text = serde_json::to_string_pretty(self)
            .expect("a spec holds strings, numbers and ordered maps");
        text.push('\n');
        text
    }

    /// Record the size of every image in `assets` that has one.
    pub fn with_images(mut self, assets: &[AssetRef]) -> Self {
        self.images = assets
            .iter()
            .filter_map(|asset| {
                let size = asset.size?;
                Some(ImageSize {
                    path: asset.path.clone(),
                    width: size.width,
                    height: size.height,
                })
            })
            .collect();
        self
    }

    /// The images as the emitter reads them: a file the site carries, with
    /// the size a document writes onto it. The file is already where the
    /// build put it, so where it came from is where it is.
    pub fn assets(&self) -> Vec<AssetRef> {
        self.images
            .iter()
            .map(|image| {
                AssetRef::new(&image.path, image.path.clone()).with_size(PixelSize {
                    width: image.width,
                    height: image.height,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use lumen_html::contract::SeedValue;

    use super::*;

    fn spec() -> ServerSpec {
        let mut seed = Seed::new();
        seed.globals.insert(
            "visitor".to_string(),
            SeedValue::Str("declared".to_string()),
        );
        ServerSpec {
            locales: vec!["en-US".to_string(), "de-DE".to_string()],
            fallback: vec!["en-US".to_string()],
            pages: vec![PageHead {
                key: "index".to_string(),
                title: Some("Welcome".to_string()),
                description: None,
                index: false,
            }],
            seed,
            policy: ServerPolicy {
                allow_hosts: vec!["api.example.com".to_string()],
                max_requests: Some(3),
                headers: vec!["authorization".to_string()],
            },
            ..ServerSpec::new(WebSpec {
                url: Some("https://example.com".to_string()),
                css: "styles.0123456789abcdef.css".to_string(),
                catalogues: [(
                    "de-DE".to_string(),
                    "locale/de-DE.0123456789abcdef.ftl".to_string(),
                )]
                .into(),
                addons: vec![crate::spec::WebAddon {
                    name: "echo".to_string(),
                    module: crate::spec::CheckedFile {
                        path: "addons/echo.0123456789abcdef/echo.js".to_string(),
                        integrity: "sha384-AAAA".to_string(),
                    },
                    styles: Vec::new(),
                    head: None,
                    config: None,
                }],
                foreign: [(
                    "echo-view".to_string(),
                    lumen_html::contract::ForeignElement {
                        html: "div".to_string(),
                        void: false,
                    },
                )]
                .into(),
                ..WebSpec::default()
            })
            .with_images(&[
                AssetRef::new("/build/logo.png", "assets/logo.png").with_size(PixelSize {
                    width: 256,
                    height: 128,
                }),
                AssetRef::new("/build/notes.txt", "assets/notes.txt"),
            ])
        }
    }

    #[test]
    fn a_spec_reads_back_as_it_was_written() {
        let spec = spec();
        let back = ServerSpec::from_json(spec.to_json().as_bytes()).expect("it reads back");
        assert_eq!(back, spec);
        // Only a file whose size was read is an image the documents size.
        assert_eq!(back.images.len(), 1);
        let assets = back.assets();
        assert_eq!(assets[0].path, "assets/logo.png");
        assert_eq!(
            assets[0].size,
            Some(PixelSize {
                width: 256,
                height: 128
            })
        );
    }

    /// The file a build writes, as this version writes it. A change to the
    /// shape changes these bytes, and a server built against the old shape
    /// would read the new file wrong, so the version has to move with it.
    const GOLDEN: &str = "tests/fixtures/lumen.site.json";

    #[test]
    fn the_spec_file_keeps_the_shape_its_version_names() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN);
        let written = spec().to_json();
        if std::env::var_os("UPDATE_GOLDENS").is_some() {
            std::fs::write(&path, &written).expect("write the golden file");
        }
        let golden = std::fs::read_to_string(&path).expect("read the golden file");
        assert!(
            written == golden,
            "{SERVER_SPEC_FILE} no longer has the shape of version {SERVER_SPEC_VERSION}. \
             Bump SERVER_SPEC_VERSION in web/src/server.rs, then rewrite {GOLDEN} with \
             UPDATE_GOLDENS=1 cargo test -p lumen-web.\n--- golden\n{golden}\n--- written\n{written}"
        );
        assert_eq!(
            ServerSpec::from_json(golden.as_bytes()).expect("the golden file reads"),
            spec()
        );
    }

    #[test]
    fn a_field_the_version_does_not_name_is_refused() {
        let json = spec().to_json();
        let extra = json.replacen('{', "{\n  \"extra\": true,", 1);
        assert!(matches!(
            ServerSpec::from_json(extra.as_bytes()),
            Err(ServerSpecError::Decode(_))
        ));
        let policy = json.replace("\"allow_hosts\"", "\"allowed_hosts\"");
        assert!(matches!(
            ServerSpec::from_json(policy.as_bytes()),
            Err(ServerSpecError::Decode(_))
        ));
        let missing = json.replace("\"per_request\": false,", "");
        assert!(
            missing != json,
            "the sample names every field of the site settings"
        );
        assert!(matches!(
            ServerSpec::from_json(missing.as_bytes()),
            Err(ServerSpecError::Decode(_))
        ));
    }

    #[test]
    fn a_spec_in_another_version_is_refused() {
        let text = spec()
            .to_json()
            .replace("\"version\": 2", "\"version\": 999");
        assert_eq!(
            ServerSpec::from_json(text.as_bytes()),
            Err(ServerSpecError::Version {
                found: 999,
                expected: SERVER_SPEC_VERSION
            })
        );
        assert!(matches!(
            ServerSpec::from_json(b"not json"),
            Err(ServerSpecError::Decode(_))
        ));
    }
}
