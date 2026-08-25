//! The `lumenc new <name> [template]` gallery.
//!
//! Two halves. Which templates exist, in which order, and what each one is for
//! lives here, so `lumenc new --list` answers with nothing on disk and no
//! network. The files a template is made of do not: each template is a
//! repository of its own under `lumen-fx`, and a Lumen release packages a copy
//! of every one beside the toolchain. `lumenc new` reads them from there.
//!
//! That is the same arrangement the candela standard library uses: a tree
//! installed next to the running executable, found by walking out from the
//! executable rather than by being compiled in. In an installed toolchain the
//! templates are `bin/templates`, beside `bin/libs`. In a checkout
//! `tools/fetch-templates.sh` downloads them into cargo's target directory,
//! which is a few directories above whichever binary is running.

use std::path::{Path, PathBuf};

/// One gallery entry: the name `lumenc new` takes and what the template is
/// for. The files come from the payload directory, under this same name.
pub struct Template {
    /// CLI name (the optional second argument to `lumenc new`).
    pub name: &'static str,
    /// One-line description shown by `lumenc new --list`.
    pub description: &'static str,
}

/// Every template, in gallery order (simplest first).
pub const TEMPLATES: &[Template] = &[
    Template {
        name: "blank",
        description: "Empty starting point: a bare <root>, a lumen.toml, nothing else.",
    },
    Template {
        name: "hello",
        description: "Smallest runnable app: one label + a script that says hi.",
    },
    Template {
        name: "counter",
        description: "Click-to-bump counter: buttons, bind-text, per-element click handlers.",
    },
    Template {
        name: "form",
        description: "Two-way bound form: input, toggle, slider, live status line.",
    },
    Template {
        name: "todo",
        description: "The canonical tutorial app: list + input + <for> loop + array signals.",
    },
    Template {
        name: "dashboard",
        description: "Stat tiles + progress bars + activity feed, driven by a timer.",
    },
    Template {
        name: "settings",
        description: "Settings panel: checkbox / radio / dropdown / slider groups + derive().",
    },
    Template {
        name: "hotkeys",
        description: "Native shell showcase: global hotkeys, tray icon, OS notifications.",
    },
];

/// Look up a template by CLI name.
pub fn find(name: &str) -> Option<&'static Template> {
    TEMPLATES.iter().find(|t| t.name == name)
}

/// Comma-separated template names for error messages / usage text.
pub fn template_names() -> String {
    TEMPLATES
        .iter()
        .map(|t| t.name)
        .collect::<Vec<_>>()
        .join(" | ")
}

/// The directory name the payload carries, wherever it is found.
const PAYLOAD: &str = "templates";

/// Why a template's files could not be read.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Nothing holding the templates was found.
    #[error(
        "the app templates are not installed. A release ships a copy of every \
         one beside lumenc, and none of these directories has them: {}. \
         In a Lumen checkout, run tools/fetch-templates.sh to download them; \
         an installed toolchain gets them back by installing it again",
        looked.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
    )]
    NoPayload {
        /// The directories that were tried, in order.
        looked: Vec<PathBuf>,
    },

    /// The payload was found, and it carries no directory for this template.
    #[error("the templates beside lumenc carry no `{name}` (looked in {})", dir.display())]
    NotInPayload {
        /// The template that was asked for.
        name: String,
        /// The payload directory that answered.
        dir: PathBuf,
    },

    /// A file could not be read or written.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file or directory the call was on.
        path: PathBuf,
        /// What the filesystem said.
        #[source]
        source: std::io::Error,
    },
}

/// How far above the executable the search looks. A cargo build puts a binary
/// one directory below the target root, a test binary two, and either of them
/// three when a target triple or an instrumented profile adds a directory of
/// its own.
const CLIMB: usize = 4;

/// Where a copy of the templates can be, in the order they are tried.
///
/// `LUMEN_TEMPLATES_DIR` names one outright, which is how a copy somewhere
/// else entirely is used. Otherwise the search walks out from the running
/// executable, resolved through symlinks first so a `~/.local/bin/lumenc`
/// link into an install prefix finds that prefix's copy. The first candidate
/// is the directory holding the executable, which is where an installed
/// toolchain keeps the templates: `bin/templates`, beside `bin/libs`. The rest
/// are the directories above it, which is where a checkout keeps them, since
/// `tools/fetch-templates.sh` writes them to the root of cargo's target
/// directory and a built binary sits below that root.
fn candidates() -> Vec<PathBuf> {
    if let Some(dir) = std::env::var_os("LUMEN_TEMPLATES_DIR").filter(|v| !v.is_empty()) {
        return vec![PathBuf::from(dir)];
    }
    let Ok(exe) = std::env::current_exe() else {
        return Vec::new();
    };
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    let Some(dir) = exe.parent() else {
        return Vec::new();
    };
    dir.ancestors()
        .take(CLIMB + 1)
        .map(|base| base.join(PAYLOAD))
        .collect()
}

/// The directory holding a copy of every template.
///
/// A candidate answers only if it carries templates this gallery lists, so an
/// unrelated `templates/` directory somewhere above the executable is walked
/// past rather than scaffolded out of.
pub fn payload_dir() -> Result<PathBuf, Error> {
    let looked = candidates();
    looked
        .iter()
        .find(|dir| holds_templates(dir))
        .cloned()
        .ok_or(Error::NoPayload { looked })
}

/// Whether `dir` carries any template the gallery lists, as a directory with
/// the `lumen.toml` every app has at its root.
fn holds_templates(dir: &Path) -> bool {
    TEMPLATES
        .iter()
        .any(|t| dir.join(t.name).join("lumen.toml").is_file())
}

/// The directory holding one template's files.
pub fn template_dir(name: &str) -> Result<PathBuf, Error> {
    let payload = payload_dir()?;
    let dir = payload.join(name);
    if dir.is_dir() {
        Ok(dir)
    } else {
        Err(Error::NotInPayload {
            name: name.to_string(),
            dir: payload,
        })
    }
}

/// Write the `name` template into `dest`, creating `dest` and every directory
/// under it.
///
/// The archive a template repository publishes holds the app tree and nothing
/// else, so every file in it is copied as it is. Returns the paths written,
/// relative to `dest`, slash-joined and sorted.
pub fn write_template(name: &str, dest: &Path) -> Result<Vec<String>, Error> {
    let source = template_dir(name)?;
    let mut written = Vec::new();
    copy_dir(&source, dest, "", &mut written)?;
    written.sort();
    Ok(written)
}

/// Copy everything under `from` into `to`, recording each file as
/// `<prefix><name>`.
fn copy_dir(from: &Path, to: &Path, prefix: &str, written: &mut Vec<String>) -> Result<(), Error> {
    std::fs::create_dir_all(to).map_err(|source| Error::Io {
        path: to.to_path_buf(),
        source,
    })?;
    let entries = std::fs::read_dir(from).map_err(|source| Error::Io {
        path: from.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| Error::Io {
            path: from.to_path_buf(),
            source,
        })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let target = to.join(&name);
        if path.is_dir() {
            copy_dir(&path, &target, &format!("{prefix}{name}/"), written)?;
        } else {
            std::fs::copy(&path, &target).map_err(|source| Error::Io {
                path: target.clone(),
                source,
            })?;
            written.push(format!("{prefix}{name}"));
        }
    }
    Ok(())
}
