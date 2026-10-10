//! Script languages, as the app sees them: a registry each host fills in.
//!
//! The engine runs scripts without knowing any language. A host lives in a
//! runtime module of its own; installing that module registers a
//! [`ScriptLanguage`] here, under the language's name, and the app assembly
//! reaches the host only through the entries of that record: install a
//! program, swap it on a hot reload, check or compile one ahead of time, read
//! the markup blocks a script writes, and reach the installed host's signals
//! and functions.
//!
//! Which module provides which language, and for which file extensions, is
//! the module's own descriptor (`lumen-language.toml`), read by
//! `lumen_modules::language`. Nothing here, and nothing in the engine, names a
//! language.

use std::path::PathBuf;

use bevy_ecs::component::Mutable;
use bevy_ecs::prelude::{Resource, World};
use lumen_core::prelude::App;
use lumen_core::warn_line;
use lumen_ir::fragment::FragmentComponent;
use lumen_ir::source_map::SourceMap;

use crate::{ScriptError, ScriptFn, ScriptHost, ScriptPrelude, ScriptValue};

/// One program, handed to a host to run.
#[derive(Debug, Clone, Default)]
pub struct ScriptProgram {
    /// The program's source text: every file of the language, concatenated in
    /// source order. Empty when the program travels as bytecode alone.
    pub source: String,
    /// The program compiled ahead of time, for a language that has a
    /// bytecode form. A host that runs bytecode reads this and ignores the
    /// source.
    pub bytecode: Option<Vec<u8>>,
    /// Where the program came from, named in a load error.
    pub uri: String,
    /// Where each piece of [`Self::source`] was read from, so a load error
    /// names the script file and line rather than [`Self::uri`].
    pub source_map: SourceMap,
    /// The app's `lib/` directory, where a native library a script imports is
    /// looked for.
    pub lib_dir: Option<PathBuf>,
    /// Script libraries the app depends on: the name a script imports under,
    /// and the directory holding its sources.
    pub import_roots: Vec<(String, PathBuf)>,
    /// The names a script's compile-time conditions are true for.
    pub cfg_flags: Vec<String>,
}

/// What an ahead-of-time check or compile is done against: the same things a
/// run hands the host, plus the functions and sources the app's modules
/// register, declared but never called.
#[derive(Debug, Clone, Copy)]
pub struct ScriptCompile<'a> {
    /// Where the program came from, named in a compile error.
    pub uri: &'a str,
    /// Where each piece of the source was read from; see
    /// [`ScriptError::relocate`].
    pub source_map: &'a SourceMap,
    /// The app's `lib/` directory.
    pub lib_dir: Option<&'a std::path::Path>,
    /// Script libraries the app depends on.
    pub import_roots: &'a [(String, PathBuf)],
    /// The names a script's compile-time conditions are true for.
    pub cfg_flags: &'a [&'a str],
    /// Functions the program may call, declared to the compiler.
    pub fns: &'a [ScriptFn],
    /// Sources the modules register, compiled ahead of the program. A host
    /// takes the ones written in its own language.
    pub preludes: &'a [ScriptPrelude],
}

/// One markup block a script writes: a fragment of markup a script
/// instantiates by key, read out of the script before anything runs.
#[derive(Debug, Clone, PartialEq)]
pub struct MarkupBlock {
    /// Byte offset of the block's body in the script, for an error that names
    /// a line.
    pub offset: usize,
    /// The block's markup, ready for the markup front-end.
    pub markup: String,
    /// The key a script instantiates the block by.
    pub key: String,
    /// The names the block's argument sites bind.
    pub args: Vec<String>,
    /// The function this block is the whole body of, when a use site can write
    /// that function as a tag.
    pub component: Option<FragmentComponent>,
}

/// A markup block the language could not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkupBlockError {
    /// Byte offset in the script the message is about.
    pub offset: usize,
    /// What is wrong.
    pub message: String,
}

/// Reaching an installed host, in the terms a caller asks in.
///
/// Every entry resolves the host resource itself, so the app owns no borrow of
/// it between calls.
#[derive(Clone, Copy)]
pub struct ScriptHostAccess {
    /// The value the script last wrote to a signal, from the host's mirror.
    pub signal: fn(&World, &str) -> Option<ScriptValue>,
    /// Call an exported function with no arguments, returning what it returned
    /// when the host has such a function.
    pub call: fn(&mut World, &str) -> Result<Option<ScriptValue>, ScriptError>,
    /// The names a caller may call.
    pub exports: fn(&World) -> Vec<String>,
}

impl ScriptHostAccess {
    /// The table an app with no script answers through: every entry reports
    /// nothing rather than the caller having to ask whether there is a host.
    pub fn absent() -> Self {
        Self {
            signal: |_, _| None,
            call: |_, _| Err(ScriptError::Runtime("no script is loaded".to_owned())),
            exports: |_| Vec::new(),
        }
    }

    /// The table for a host stored as the resource `H`. `exports` is the
    /// host's own rule for what a caller may call: [`ScriptHost`] carries no
    /// export list, so a host without one answers with an empty list.
    pub fn of<H>(exports: fn(&World) -> Vec<String>) -> Self
    where
        H: ScriptHost + Resource<Mutability = Mutable>,
    {
        Self {
            signal: signal_of::<H>,
            call: call_of::<H>,
            exports,
        }
    }
}

/// Read `name` from the host's signal mirror.
fn signal_of<H>(world: &World, name: &str) -> Option<ScriptValue>
where
    H: ScriptHost + Resource<Mutability = Mutable>,
{
    world.get_resource::<H>().and_then(|h| h.mirror_get(name))
}

/// Call `name` with no arguments. Commands the call queued are put back so the
/// next tick carries them, exactly as the app's own dispatchers do.
fn call_of<H>(world: &mut World, name: &str) -> Result<Option<ScriptValue>, ScriptError>
where
    H: ScriptHost + Resource<Mutability = Mutable>,
{
    let Some(mut host) = world.get_resource_mut::<H>() else {
        return Err(ScriptError::Runtime("no script is loaded".to_owned()));
    };
    match host.call(name, &[]) {
        Ok(outcome) => {
            host.push_commands(outcome.commands);
            Ok(outcome.ret.filter(|_| outcome.found))
        }
        Err(failure) => {
            host.push_commands(failure.commands);
            Err(failure.error)
        }
    }
}

/// Install a host over a program. `multi_host` is true when the app runs more
/// than one language, which a host needs to keep its signal mirror current
/// with the others.
pub type InstallFn = fn(&mut App, ScriptProgram, bool);
/// Swap a live host's program for a hot reload: `(world, source, uri, map)`,
/// `map` saying where each piece of `source` was read from. `None` when the
/// host is not installed.
pub type ReloadFn = fn(&mut World, &str, &str, &SourceMap) -> Option<Result<(), ScriptError>>;
/// Compile-check a program without running it: `(source, against)`.
pub type CheckFn = fn(&str, &ScriptCompile<'_>) -> Result<(), String>;
/// Compile a program to bytecode: `(source, against)`, giving the image and
/// the compiler's warnings, one line each.
pub type CompileFn = fn(&str, &ScriptCompile<'_>) -> Result<(Vec<u8>, Vec<String>), String>;
/// The names a bytecode image can be called by, with `fns` bound beside the
/// builtins and none of them called.
pub type ImageExportsFn = fn(&[u8], &[ScriptFn]) -> Result<Vec<String>, String>;
/// Read the markup blocks a script writes.
pub type MarkupBlocksFn = fn(&str) -> Result<Vec<MarkupBlock>, MarkupBlockError>;

/// One script language, as a host module registers it.
///
/// Only [`Self::install`] and [`Self::access`] are required: a host that runs
/// precompiled bytecode alone has no compiler to check, compile, or reload
/// with, and leaves those entries empty.
#[derive(Clone, Copy)]
pub struct ScriptLanguage {
    /// The language's name, the one `[script] engine` and the module's
    /// descriptor use.
    pub name: &'static str,
    /// Install the host over a program.
    pub install: InstallFn,
    /// Swap the program for a hot reload.
    pub reload: Option<ReloadFn>,
    /// Reach the installed host.
    pub access: fn() -> ScriptHostAccess,
    /// Compile-check a program.
    pub check: Option<CheckFn>,
    /// Compile a program to bytecode.
    pub compile: Option<CompileFn>,
    /// Read a bytecode image's exports.
    pub image_exports: Option<ImageExportsFn>,
    /// Read the markup blocks a script writes.
    pub markup_blocks: Option<MarkupBlocksFn>,
}

impl ScriptLanguage {
    /// A language whose host installs with `install` and is reached through
    /// `access`, with every optional entry empty.
    pub fn new(name: &'static str, install: InstallFn, access: fn() -> ScriptHostAccess) -> Self {
        Self {
            name,
            install,
            reload: None,
            access,
            check: None,
            compile: None,
            image_exports: None,
            markup_blocks: None,
        }
    }
}

impl std::fmt::Debug for ScriptLanguage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptLanguage")
            .field("name", &self.name)
            .field("reload", &self.reload.is_some())
            .field("check", &self.check.is_some())
            .field("compile", &self.compile.is_some())
            .field("image_exports", &self.image_exports.is_some())
            .field("markup_blocks", &self.markup_blocks.is_some())
            .finish()
    }
}

/// The languages the app's host modules registered, in registration order.
#[derive(Resource, Default, Debug, Clone)]
pub struct ScriptLanguages(Vec<ScriptLanguage>);

impl ScriptLanguages {
    /// The language registered under `name`.
    pub fn get(&self, name: &str) -> Option<&ScriptLanguage> {
        self.0.iter().find(|l| l.name == name)
    }

    /// Every registered language.
    pub fn iter(&self) -> impl Iterator<Item = &ScriptLanguage> {
        self.0.iter()
    }

    /// Whether no language is registered.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Register `language`. A second registration of a name already present
    /// is refused with a warning, so the first module to provide a language
    /// is the one that runs it.
    pub fn add(&mut self, language: ScriptLanguage) {
        if self.get(language.name).is_some() {
            warn_line!(
                "lumen-runtime: script language `{}` is registered twice; the first \
                 registration runs it",
                language.name
            );
            return;
        }
        self.0.push(language);
    }
}

/// Registering a script language on an [`App`].
pub trait ScriptLanguageAppExt {
    /// Register `language`. See [`ScriptLanguages::add`].
    fn add_script_language(&mut self, language: ScriptLanguage) -> &mut Self;
}

impl ScriptLanguageAppExt for App {
    fn add_script_language(&mut self, language: ScriptLanguage) -> &mut Self {
        self.world
            .get_resource_or_insert_with(ScriptLanguages::default)
            .add(language);
        self
    }
}

/// No host is registered for the language a program names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownLanguage {
    /// The language the program names.
    pub language: String,
    /// The languages a host is registered for, so the message says what this
    /// build can run instead.
    pub registered: Vec<String>,
}

impl std::fmt::Display for UnknownLanguage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no script host for language \"{}\"", self.language)?;
        if self.registered.is_empty() {
            write!(f, "; no script host is installed")
        } else {
            write!(f, "; this build runs {}", self.registered.join(", "))
        }
    }
}

impl std::error::Error for UnknownLanguage {}

/// Install the host registered for `language` over `program`, and return the
/// table that reaches it.
///
/// # Errors
///
/// No module registered a host for `language`.
pub fn install_program(
    app: &mut App,
    language: &str,
    program: ScriptProgram,
    multi_host: bool,
) -> Result<ScriptHostAccess, UnknownLanguage> {
    let languages = app.world.get_resource::<ScriptLanguages>();
    let Some(entry) = languages.and_then(|l| l.get(language)).copied() else {
        return Err(UnknownLanguage {
            language: language.to_owned(),
            registered: languages
                .map(|l| l.iter().map(|entry| entry.name.to_owned()).collect())
                .unwrap_or_default(),
        });
    };
    (entry.install)(app, program, multi_host);
    Ok((entry.access)())
}

/// The 1-based line and column of byte `offset` in `src`.
pub fn line_col(src: &str, offset: usize) -> (usize, usize) {
    let before = &src[..offset.min(src.len())];
    let line = before.matches('\n').count() + 1;
    let col = before.rfind('\n').map_or(before.chars().count(), |nl| {
        before[nl + 1..].chars().count()
    }) + 1;
    (line, col)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn install(_app: &mut App, _program: ScriptProgram, _multi: bool) {}

    #[test]
    fn the_first_registration_of_a_name_is_the_one_kept() {
        let mut app = App::new();
        let mut first = ScriptLanguage::new("toy", install, ScriptHostAccess::absent);
        first.check = Some(|_, _| Ok(()));
        app.add_script_language(first);
        app.add_script_language(ScriptLanguage::new(
            "toy",
            install,
            ScriptHostAccess::absent,
        ));
        let languages = app.world.resource::<ScriptLanguages>();
        assert_eq!(languages.iter().count(), 1);
        assert!(languages.get("toy").and_then(|l| l.check).is_some());
    }

    #[test]
    fn a_program_for_an_unregistered_language_is_refused_by_name() {
        let mut app = App::new();
        let refusal = install_program(&mut app, "toy", ScriptProgram::default(), false)
            .err()
            .expect("nothing registered toy");
        assert_eq!(
            refusal.to_string(),
            "no script host for language \"toy\"; no script host is installed"
        );

        app.add_script_language(ScriptLanguage::new(
            "other",
            install,
            ScriptHostAccess::absent,
        ));
        let refusal = install_program(&mut app, "toy", ScriptProgram::default(), false)
            .err()
            .expect("still nothing registered toy");
        assert_eq!(
            refusal.to_string(),
            "no script host for language \"toy\"; this build runs other"
        );
    }

    #[test]
    fn line_and_column_count_from_one() {
        let src = "ab\ncd\nef";
        assert_eq!(line_col(src, 0), (1, 1));
        assert_eq!(line_col(src, 4), (2, 2));
        assert_eq!(line_col(src, 99), (3, 3));
    }
}
