//! Browser add-ons: a `[dependencies]` entry whose directory holds a
//! `lumen-addon.toml`.
//!
//! An add-on is data, not a library. It is a JavaScript module a page loads
//! beside the runtime, with optional stylesheets, a script that runs before
//! the page paints, and files the module reads, all described by the
//! descriptor at the package root:
//!
//! ```toml
//! [addon]
//! namespace = "echo"
//! module = "echo.js"
//! styles = ["echo.css"]
//!
//! [[function]]
//! name = "shout"
//! params = ["text: string"]
//! returns = "string"
//!
//! [[function]]
//! name = "later"
//! params = ["text: string", "ms: int"]
//! async = "on_echo"
//!
//! [[element]]
//! tag = "echo-view"
//! html = "div"
//! ```
//!
//! Reading one is a compile's job: the descriptor becomes the
//! [`lumen_ir::addon::Addon`] the compiled app carries, and the files become
//! the site's. No engine opens anything here.

use std::path::{Component, Path, PathBuf};

use lumen_ir::addon::{Addon, AddonElement, AddonFunction, AddonParam};
use serde::Deserialize;

use crate::Target;

/// The descriptor's file name, at the package root.
pub const ADDON_MANIFEST: &str = "lumen-addon.toml";

/// Module exports an add-on cannot name a function after, because the page
/// reads them as the module's own hooks.
pub const RESERVED_EXPORTS: &[&str] = &["install", "elements", "default"];

/// Script namespaces an add-on cannot take: the runtime's own, the embedder's,
/// and the one candela keeps for its standard library.
pub const RESERVED_NAMESPACES: &[&str] = &["lumen", "native", "fs"];

/// An add-on read off disk: what the compiled app carries, and the files the
/// site ships.
#[derive(Debug, Clone, PartialEq)]
pub struct AddonPackage {
    /// The functions and elements, as the compiled app carries them.
    pub addon: Addon,
    /// The package root.
    pub dir: PathBuf,
    /// The JavaScript module, relative to [`Self::dir`].
    pub module: String,
    /// Stylesheets the pages link, relative to [`Self::dir`], in order.
    pub styles: Vec<String>,
    /// A classic script the pages run before they paint, relative to
    /// [`Self::dir`].
    pub head: Option<String>,
    /// Other files the module reads at run time, relative to [`Self::dir`]. A
    /// directory stands for everything under it.
    pub files: Vec<String>,
    /// True when the dependency is a runtime module on every target but the
    /// web, and this add-on is that module's page implementation.
    pub native: bool,
    /// The `config` table the app's dependency entry gave it, which a page
    /// hands the module at install. Empty until the dependency is known:
    /// [`read_addon`] reads the package alone.
    pub config: toml::Table,
}

/// True when `dir` is an add-on package.
pub fn is_addon(dir: &Path) -> bool {
    dir.join(ADDON_MANIFEST).is_file()
}

/// True when `dir` is an add-on a build for `target` takes as one.
///
/// A web build takes every add-on. Any other build takes one unless its
/// descriptor says `native = true`: that package is a runtime module there,
/// and the dependency is the module's business, not the add-on's. A
/// descriptor that does not parse counts as an add-on on every target, so the
/// build that reads it in full is the one that reports it.
pub fn serves(dir: &Path, target: Target) -> bool {
    if !is_addon(dir) {
        return false;
    }
    if target == Target::Web {
        return true;
    }
    #[derive(Deserialize)]
    struct Probe {
        addon: ProbeHeader,
    }
    #[derive(Deserialize)]
    struct ProbeHeader {
        #[serde(default)]
        native: bool,
    }
    let native = std::fs::read_to_string(dir.join(ADDON_MANIFEST))
        .ok()
        .and_then(|text| toml::from_str::<Probe>(&text).ok())
        .is_some_and(|probe| probe.addon.native);
    !native
}

/// Read the add-on at `dir`, declared in `[dependencies]` as `name`.
///
/// # Errors
///
/// The descriptor is missing or does not parse, names a file the package does
/// not hold or one outside it, or declares a function, a namespace or an
/// element this format refuses. Every message names the add-on.
pub fn read_addon(name: &str, dir: &Path) -> Result<AddonPackage, String> {
    let path = dir.join(ADDON_MANIFEST);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("add-on '{name}': read {}: {e}", path.display()))?;
    parse_addon(name, dir, &text)
}

/// The descriptor as it is written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Descriptor {
    addon: Header,
    #[serde(default)]
    function: Vec<FunctionDecl>,
    #[serde(default)]
    element: Vec<ElementDecl>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    namespace: String,
    module: String,
    #[serde(default)]
    styles: Vec<String>,
    #[serde(default)]
    head: Option<String>,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    native: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FunctionDecl {
    name: String,
    #[serde(default)]
    params: Vec<String>,
    #[serde(default)]
    returns: Option<String>,
    #[serde(default, rename = "async")]
    event: Option<String>,
    #[serde(default)]
    doc: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ElementDecl {
    tag: String,
    html: String,
    #[serde(default)]
    void: bool,
}

/// Parse a descriptor's text, checking the files it names against `dir`.
fn parse_addon(name: &str, dir: &Path, text: &str) -> Result<AddonPackage, String> {
    let fail = |message: String| format!("add-on '{name}': {message}");
    let descriptor: Descriptor =
        toml::from_str(text).map_err(|e| fail(format!("{ADDON_MANIFEST}: {e}")))?;
    let header = descriptor.addon;

    if !is_identifier(&header.namespace) {
        return Err(fail(format!(
            "namespace `{}` must be lowercase letters, digits and underscores, starting with a \
             letter",
            header.namespace
        )));
    }
    if RESERVED_NAMESPACES.contains(&header.namespace.as_str()) {
        return Err(fail(format!(
            "namespace `{}` is taken by the runtime; pick another",
            header.namespace
        )));
    }

    let file = |path: &str, what: &str| -> Result<String, String> {
        package_file(dir, path).map_err(|why| fail(format!("{what} `{path}` {why}")))
    };
    let module = file(&header.module, "module")?;
    if !(module.ends_with(".js") || module.ends_with(".mjs")) {
        return Err(fail(format!(
            "module `{module}` must be a JavaScript module (.js or .mjs)"
        )));
    }
    let styles = header
        .styles
        .iter()
        .map(|s| file(s, "stylesheet"))
        .collect::<Result<Vec<_>, _>>()?;
    let head = header
        .head
        .as_deref()
        .map(|h| file(h, "head script"))
        .transpose()?;
    let files = header
        .files
        .iter()
        .map(|f| file(f, "file"))
        .collect::<Result<Vec<_>, _>>()?;

    let mut functions: Vec<AddonFunction> = Vec::with_capacity(descriptor.function.len());
    for decl in descriptor.function {
        let qualified = format!("{}::{}", header.namespace, decl.name);
        if !is_identifier(&decl.name) {
            return Err(fail(format!(
                "function `{}` must be lowercase letters, digits and underscores, starting with \
                 a letter",
                decl.name
            )));
        }
        if RESERVED_EXPORTS.contains(&decl.name.as_str()) {
            return Err(fail(format!(
                "function `{}` shares its name with a module hook; rename it",
                decl.name
            )));
        }
        if functions.iter().any(|f| f.name == decl.name) {
            return Err(fail(format!("function `{}` is declared twice", decl.name)));
        }
        let mut params = Vec::with_capacity(decl.params.len());
        for spelling in &decl.params {
            let Some((param, ty)) = spelling.split_once(':') else {
                return Err(fail(format!(
                    "{qualified}: parameter `{spelling}` must be written `name: type`"
                )));
            };
            let (param, ty) = (param.trim(), ty.trim());
            if !is_identifier(param) || ty.is_empty() {
                return Err(fail(format!(
                    "{qualified}: parameter `{spelling}` must be written `name: type`"
                )));
            }
            params.push(AddonParam {
                name: param.to_owned(),
                ty: ty.to_owned(),
            });
        }
        if let Some(event) = &decl.event {
            if decl.returns.is_some() {
                return Err(fail(format!(
                    "{qualified} is async, so its result arrives as `{event}` and it returns \
                     nothing; drop `returns`"
                )));
            }
            if !is_identifier(event) {
                return Err(fail(format!(
                    "{qualified}: event `{event}` must be lowercase letters, digits and \
                     underscores, starting with a letter"
                )));
            }
        }
        functions.push(AddonFunction {
            name: decl.name,
            params,
            returns: decl.returns.unwrap_or_else(|| "null".to_owned()),
            event: decl.event,
            doc: decl.doc,
        });
    }

    let mut elements: Vec<AddonElement> = Vec::with_capacity(descriptor.element.len());
    for decl in descriptor.element {
        crate::validate_tag(name, &decl.tag)?;
        if elements.iter().any(|e| e.tag == decl.tag) {
            return Err(fail(format!("element `{}` is declared twice", decl.tag)));
        }
        if !is_element_name(&decl.html) {
            return Err(fail(format!(
                "element `{}`: `html = \"{}\"` must be an HTML element name",
                decl.tag, decl.html
            )));
        }
        elements.push(AddonElement {
            tag: decl.tag,
            html: decl.html,
            void: decl.void,
        });
    }

    Ok(AddonPackage {
        addon: Addon {
            name: name.to_owned(),
            namespace: header.namespace,
            functions,
            elements,
        },
        dir: dir.to_path_buf(),
        module,
        styles,
        head,
        files,
        native: header.native,
        config: toml::Table::new(),
    })
}

/// A path the package holds, relative to its root and inside it, written with
/// `/` whatever the platform.
fn package_file(dir: &Path, path: &str) -> Result<String, &'static str> {
    let relative = Path::new(path);
    if path.is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err("must be a path inside the package");
    }
    if !dir.join(relative).exists() {
        return Err("is not in the package");
    }
    Ok(relative
        .components()
        .filter_map(|c| match c {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/"))
}

/// A name a script can write: lowercase ASCII letters, digits and
/// underscores, starting with a letter.
fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// A name HTML writes an element with: lowercase ASCII letters, digits and
/// dashes, starting with a letter.
fn is_element_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A package directory holding `files`, for one test.
    fn package(test: &str, files: &[&str]) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lumen-modules-addon-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for file in files {
            let path = dir.join(file);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
            std::fs::write(&path, "").expect("write");
        }
        dir
    }

    const ECHO: &str = r#"
[addon]
namespace = "echo"
module = "echo.js"
styles = ["echo.css"]
head = "early.js"
files = ["assets"]

[[function]]
name = "shout"
params = ["text: string"]
returns = "string"
doc = "Upper-case it."

[[function]]
name = "later"
params = ["text: string", "ms : int"]
async = "on_echo"

[[element]]
tag = "echo-view"
html = "div"
"#;

    #[test]
    fn a_descriptor_reads_into_the_addon_and_its_files() {
        let dir = package(
            "reads",
            &["echo.js", "echo.css", "early.js", "assets/logo.svg"],
        );
        std::fs::write(dir.join(ADDON_MANIFEST), ECHO).unwrap();
        assert!(is_addon(&dir));
        let package = read_addon("echo", &dir).unwrap();
        assert_eq!(package.module, "echo.js");
        assert_eq!(package.styles, ["echo.css"]);
        assert_eq!(package.head.as_deref(), Some("early.js"));
        assert_eq!(package.files, ["assets"]);
        let addon = package.addon;
        assert_eq!(addon.name, "echo");
        assert_eq!(addon.namespace, "echo");
        assert_eq!(addon.functions[0].returns, "string");
        assert_eq!(addon.functions[0].event, None);
        assert_eq!(addon.functions[1].returns, "null");
        assert_eq!(addon.functions[1].event.as_deref(), Some("on_echo"));
        assert_eq!(addon.functions[1].params[1].name, "ms");
        assert_eq!(addon.functions[1].params[1].ty, "int");
        assert_eq!(addon.elements[0].html, "div");
        assert!(!addon.elements[0].void);
    }

    fn refusal(test: &str, text: &str) -> String {
        let dir = package(
            test,
            &["echo.js", "echo.css", "early.js", "assets/logo.svg"],
        );
        parse_addon("echo", &dir, text).unwrap_err()
    }

    #[test]
    fn a_file_outside_the_package_is_refused() {
        let err = refusal(
            "outside",
            "[addon]\nnamespace = \"echo\"\nmodule = \"../echo.js\"\n",
        );
        assert!(err.contains("must be a path inside the package"), "{err}");
        let err = refusal(
            "missing",
            "[addon]\nnamespace = \"echo\"\nmodule = \"gone.js\"\n",
        );
        assert!(err.contains("is not in the package"), "{err}");
    }

    #[test]
    fn a_function_named_after_a_hook_is_refused() {
        let err = refusal(
            "hook",
            "[addon]\nnamespace = \"echo\"\nmodule = \"echo.js\"\n[[function]]\nname = \"install\"\n",
        );
        assert!(err.contains("module hook"), "{err}");
    }

    #[test]
    fn the_runtimes_namespaces_are_refused() {
        for ns in RESERVED_NAMESPACES {
            let err = refusal(
                &format!("ns-{ns}"),
                &format!("[addon]\nnamespace = \"{ns}\"\nmodule = \"echo.js\"\n"),
            );
            assert!(err.contains("taken by the runtime"), "{err}");
        }
    }

    #[test]
    fn an_async_function_returns_nothing() {
        let err = refusal(
            "async-returns",
            "[addon]\nnamespace = \"echo\"\nmodule = \"echo.js\"\n[[function]]\nname = \
             \"later\"\nreturns = \"int\"\nasync = \"on_echo\"\n",
        );
        assert!(err.contains("drop `returns`"), "{err}");
    }

    #[test]
    fn a_parameter_is_written_name_colon_type() {
        let err = refusal(
            "param",
            "[addon]\nnamespace = \"echo\"\nmodule = \"echo.js\"\n[[function]]\nname = \
             \"shout\"\nparams = [\"string\"]\n",
        );
        assert!(err.contains("must be written `name: type`"), "{err}");
    }

    #[test]
    fn a_built_in_tag_cannot_be_an_addon_element() {
        let err = refusal(
            "tag",
            "[addon]\nnamespace = \"echo\"\nmodule = \"echo.js\"\n[[element]]\ntag = \
             \"button\"\nhtml = \"div\"\n",
        );
        assert!(err.contains("built-in tag"), "{err}");
    }

    #[test]
    fn a_native_addon_serves_the_web_build_alone() {
        let dir = package("native", &["echo.js"]);
        std::fs::write(
            dir.join(ADDON_MANIFEST),
            "[addon]\nnamespace = \"echo\"\nmodule = \"echo.js\"\nnative = true\n",
        )
        .unwrap();
        assert!(read_addon("echo", &dir).unwrap().native);
        assert!(serves(&dir, Target::Web));
        assert!(!serves(&dir, Target::Desktop));

        let plain = package("plain", &["echo.js"]);
        std::fs::write(
            plain.join(ADDON_MANIFEST),
            "[addon]\nnamespace = \"echo\"\nmodule = \"echo.js\"\n",
        )
        .unwrap();
        assert!(!read_addon("echo", &plain).unwrap().native);
        for target in Target::ALL {
            assert!(serves(&plain, target));
        }
        assert!(!serves(&plain.join("nowhere"), Target::Web));
    }

    #[test]
    fn an_unknown_key_is_refused() {
        let err = refusal(
            "unknown",
            "[addon]\nnamespace = \"echo\"\nmodule = \"echo.js\"\nversion = \"1\"\n",
        );
        assert!(err.contains("unknown field `version`"), "{err}");
    }
}
