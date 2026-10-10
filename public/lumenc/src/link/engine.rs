//! An engine of the app's own for a folder package.
//!
//! A folder package ships the engine as a shared library beside the
//! executable: `liblumen_engine` on Linux and macOS, which `liblumen` and every
//! runtime module link against, and `lumen.dll` on Windows, where the engine is
//! compiled into the C library itself. The toolchain's copy carries every
//! optional capability (the tray, notifications, dialogs, the HTTP client, the
//! developer tools) and exports every symbol its crate graph defines, which is
//! most of its size.
//!
//! The release publishes the link that produced it as an engine kit (see
//! [`lumen_modules::link_kit`]), and replaying that link is how a package gets
//! an engine of its own, with no compiler involved:
//!
//! - Each capability the app uses is forced onto the line by its register
//!   symbol, and nothing else names one, so the others are left out. Which
//!   ones the app uses is [`CapabilityChoice::select`]'s answer.
//! - On Linux and macOS the export list is replaced with the names the files
//!   shipped beside the engine resolve against it (the keep list), each one
//!   forced in, and section garbage collection drops everything nothing
//!   reaches. What the toolchain's engine exported for code nobody ships stops
//!   being kept alive.
//! - On Windows `lumen.dll` exports the C ABI and nothing more, so its own
//!   export list stays, less the register symbols of the capabilities left
//!   out. The script host modules the app's program runs on are forced in
//!   the way a capability is, since no file beside `lumen.dll` could be
//!   opened; on Linux and macOS they ship beside the engine and count among
//!   the consumers instead.
//!
//! The engine build id is untouched by a relink, so the runtime modules
//! shipped beside the engine still pass the loader's handshake.
//!
//! When there is no kit to replay - none published for this release, none
//! downloadable, another platform's package, a toolchain built locally - the
//! toolchain's engine travels whole and the package step says so in one line.
//! An app whose `[capabilities]` turns something off is refused then rather
//! than shipped with what it asked to leave out.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use lumen_ir::artifact::UnlinkedCapability;
use lumen_modules::link_kit::{KitCapability, KitModule, LinkArg, Manifest};
use lumen_runtime::modules::DependenciesCfg;
use object::{BinaryFormat, Object, ObjectSymbol};

use crate::link::kit::{self, CapabilityChoice, KitKind, Library};
use crate::package::cli::Target;
use crate::package::release;

/// Everything the engine step of one package works from.
pub(crate) struct EngineJob<'a> {
    /// The platform being packaged.
    pub(crate) target: Target,
    /// `--lib-dir`, which a static kit is looked for in; an engine kit is not.
    pub(crate) lib_dir: Option<&'a Path>,
    /// The toolchain's own engine: `liblumen_engine` on Linux and macOS,
    /// `lumen.dll` on Windows.
    pub(crate) full: &'a Path,
    /// Where the package's engine goes.
    pub(crate) out: &'a Path,
    /// Every file shipped beside the engine that resolves symbols against it:
    /// `liblumen`, the Rust standard library, each runtime module. Empty on
    /// Windows, where nothing does.
    pub(crate) consumers: &'a [PathBuf],
    /// The runtime modules the app needs that an engine kit can link in: on
    /// Windows, the script hosts its program runs on, which nothing beside
    /// `lumen.dll` could load. A kit links the ones its record carries and
    /// leaves the rest to the files staged beside the engine.
    pub(crate) modules: &'a DependenciesCfg,
    /// What decides the capabilities.
    pub(crate) choice: &'a CapabilityChoice<'a>,
}

/// The engine a package ended up with.
#[derive(Debug)]
pub(crate) enum Engine {
    /// Relinked for the app.
    Relinked {
        /// The capabilities it carries, in the kit's order.
        linked: Vec<String>,
        /// The capabilities left out, in the kit's order.
        left_out: Vec<String>,
        /// What the app's artifact records about the ones left out.
        unlinked: Vec<UnlinkedCapability>,
    },
    /// The toolchain's engine, whole, for the reason given.
    Full {
        /// Why no relink happened.
        why: String,
    },
}

impl Engine {
    /// What the app's artifact records about the capabilities its engine was
    /// linked without.
    pub(crate) fn unlinked(&self) -> Vec<UnlinkedCapability> {
        match self {
            Engine::Relinked { unlinked, .. } => unlinked.clone(),
            Engine::Full { .. } => Vec::new(),
        }
    }

    /// The part of the package summary that describes the engine.
    pub(crate) fn summary(&self) -> String {
        match self {
            Engine::Relinked {
                linked, left_out, ..
            } => format!(
                ", engine linked for the app with {} ({}) and without {} ({})",
                count(linked.len()),
                list(linked),
                count(left_out.len()),
                list(left_out)
            ),
            Engine::Full { .. } => ", the full engine".to_string(),
        }
    }
}

fn count(n: usize) -> String {
    format!("{n} capabilit{}", if n == 1 { "y" } else { "ies" })
}

fn list(names: &[String]) -> String {
    if names.is_empty() {
        "none".to_string()
    } else {
        names.join(", ")
    }
}

/// Why the step could not relink: either the package carries the full engine
/// instead, or the package itself cannot be made.
enum Stop {
    /// The full engine travels, and the reason is said once.
    Fallback(String),
    /// The package fails with this message.
    Fail(String),
}

impl From<std::io::Error> for Stop {
    fn from(e: std::io::Error) -> Self {
        Stop::Fail(e.to_string())
    }
}

/// Put the package's engine at `job.out`: relinked for the app when an engine
/// kit for this toolchain can be replayed here, the toolchain's own otherwise.
pub(crate) fn engine_for_app(job: &EngineJob<'_>) -> Result<Engine, String> {
    match relink(job) {
        Ok(engine) => Ok(engine),
        Err(Stop::Fail(e)) => Err(e),
        Err(Stop::Fallback(why)) => {
            let engine = without_kit(job.choice, why)?;
            std::fs::copy(job.full, job.out).map_err(|e| {
                format!("copy {} -> {}: {e}", job.full.display(), job.out.display())
            })?;
            Ok(engine)
        }
    }
}

/// The answer for a package that carries the toolchain's engine whole, for
/// the reason `why`: refused when `[capabilities]` turns anything off, since
/// nothing can be left out of that engine and the table is never ignored.
pub(crate) fn without_kit(choice: &CapabilityChoice<'_>, why: String) -> Result<Engine, String> {
    check_names(choice)?;
    if let Some(name) = choice.turns_any_off() {
        return Err(format!(
            "[capabilities] leaves {name} out, and this package carries the full engine: \
             {why}. Remove the entry to package the full engine, or package where the \
             engine kit can be replayed."
        ));
    }
    Ok(Engine::Full { why })
}

/// A `[capabilities]` name nothing registers is a misspelling, whichever
/// engine ships. With no kit to ask, this `lumenc`'s own registry, which
/// links every capability, answers.
fn check_names(choice: &CapabilityChoice<'_>) -> Result<(), String> {
    let registered: Vec<KitCapability> = lumen_capability::registered()
        .iter()
        .map(KitCapability::from)
        .collect();
    if registered.is_empty() {
        return Ok(());
    }
    lumen_modules::link_kit::select_capabilities(
        &registered,
        choice.sources,
        choice.config,
        choice.requested,
        choice.opaque,
    )
    .map(|_| ())
}

fn relink(job: &EngineJob<'_>) -> Result<Engine, Stop> {
    if job.target != Target::host() {
        return Err(Stop::Fallback(format!(
            "an engine for {} is relinked on a {} machine, with its linker",
            job.target.name(),
            job.target.name()
        )));
    }
    let windows = job.target.rust_triple().contains("-windows-");
    let full = std::fs::read(job.full)?;
    let toolchain_id = build_id_in(&full);
    let named = std::env::var_os(KitKind::SharedEngine.dir_env()).is_some_and(|v| !v.is_empty());
    if !named {
        // A kit is fetched from the release this toolchain came from, and
        // only a toolchain that is that release's build can replay it: a
        // download that cannot match is not worth making.
        let published = release::resolve().map_err(|e| Stop::Fallback(e.to_string()))?;
        if !published.is_build_of(release::current()) {
            return Err(Stop::Fallback(format!(
                "this lumenc is {}, not {published}, so no published engine kit matches it; \
                 point {} at a kit recorded from this build",
                release::current(),
                KitKind::SharedEngine.dir_env()
            )));
        }
    }
    let (kit, manifest) = kit::open(KitKind::SharedEngine, job.target, job.lib_dir)
        .map_err(|e| Stop::Fallback(format!("no engine kit: {e}")))?;
    check_matches(&manifest, toolchain_id.as_deref(), windows).map_err(Stop::Fallback)?;

    let selected = job
        .choice
        .select(&manifest.capabilities)
        .map_err(Stop::Fail)?;
    let left_out: Vec<String> = manifest
        .capabilities
        .iter()
        .filter(|c| !selected.iter().any(|s| s.name == c.name))
        .map(|c| c.name.clone())
        .collect();
    let registers: Vec<String> = manifest
        .capabilities
        .iter()
        .map(|c| c.register_symbol.clone())
        .collect();

    let exports = job.out.with_extension("exports");
    let keep = if windows {
        let recorded = recorded_export_list(&kit, &manifest)?;
        let dropped = entries_left_out(
            &manifest.capabilities,
            &manifest.modules,
            &selected,
            job.modules,
        );
        std::fs::write(
            &exports,
            filter_def(&std::fs::read_to_string(recorded)?, &dropped),
        )?;
        Vec::new()
    } else {
        let mut consumers = Vec::with_capacity(job.consumers.len());
        for file in job.consumers {
            consumers.push(
                std::fs::read(file)
                    .map_err(|e| Stop::Fallback(format!("cannot read {}: {e}", file.display())))?,
            );
        }
        let mut keep = keep_list(&full, &consumers, &registers).map_err(Stop::Fallback)?;
        // Each selected capability's register symbol stays exported, so the
        // engine says which capabilities it carries to anyone who reads its
        // symbol table, and the export check below proves each one linked.
        keep.extend(selected.iter().map(|c| c.register_symbol.clone()));
        let macho = matches!(manifest.driver.flavor.as_str(), "darwin");
        std::fs::write(&exports, export_list(&keep, macho))?;
        keep
    };

    let library = Library {
        exports: Some(&exports),
        keep: &keep,
        unforced_modules_stay: true,
    };
    let linked_in = DependenciesCfg(
        job.modules
            .0
            .iter()
            .filter(|dep| manifest.modules.iter().any(|m| m.name == dep.name))
            .cloned()
            .collect(),
    );
    let linked = kit::plan(
        &kit, &manifest, &linked_in, &selected, &library, job.out, job.out,
    )
    .and_then(|plan| {
        kit::link(&plan)?;
        kit::finish(&plan, &[])
    });
    let _ = std::fs::remove_file(&exports);
    if let Err(e) = linked {
        let _ = std::fs::remove_file(job.out);
        // The linker's first few words are what tell a missing system
        // library from a missing compiler; the rest is noise in a notice.
        let said: Vec<&str> = e
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .take(4)
            .collect();
        return Err(Stop::Fallback(format!(
            "the engine kit's link did not complete: {}",
            said.join(" ")
        )));
    }
    if !windows {
        check_exports(job.out, &keep).map_err(|e| {
            let _ = std::fs::remove_file(job.out);
            Stop::Fallback(e)
        })?;
    }

    Ok(Engine::Relinked {
        linked: selected.iter().map(|c| c.name.clone()).collect(),
        left_out,
        unlinked: kit::unlinked(&manifest.capabilities, &selected),
    })
}

/// Whether the kit is the link of the engine this toolchain ships. On Linux
/// and macOS the engine build id says so exactly, and it is what every runtime
/// module beside the engine was compiled against. `lumen.dll` carries none,
/// so on Windows the release the kit was written by stands in for it.
fn check_matches(
    manifest: &Manifest,
    toolchain: Option<&str>,
    windows: bool,
) -> Result<(), String> {
    if windows {
        if manifest.lumen_version != release::current() {
            return Err(format!(
                "the engine kit is from Lumen {} and this toolchain is {}",
                manifest.lumen_version,
                release::current()
            ));
        }
        return Ok(());
    }
    match (manifest.build_id.as_deref(), toolchain) {
        (Some(kit), Some(engine)) if kit == engine => Ok(()),
        (kit, engine) => Err(format!(
            "the engine kit records {} and the toolchain's engine is {}",
            kit.unwrap_or("no engine build"),
            engine.unwrap_or("no engine build")
        )),
    }
}

/// The export list the kit recorded, which on Windows is the C ABI the
/// relinked library still exports.
fn recorded_export_list(kit: &Path, manifest: &Manifest) -> Result<PathBuf, Stop> {
    manifest
        .args
        .iter()
        .find_map(|arg| match arg {
            LinkArg::ExportList { path, .. } => Some(kit.join("stage").join(path)),
            _ => None,
        })
        .ok_or_else(|| Stop::Fallback("the engine kit records no export list".to_string()))
}

/// The register entries a replay leaves out of a recorded export list.
///
/// The recorded list names the register entry of everything the recorded
/// line linked: each of the kit's `capabilities` and `offered` modules. A
/// capability the app goes without, and a module it does not run on, is not
/// on the replayed line, so its entry leaves the list with it: an export
/// nothing defines fails the link. `selected` are the capabilities the app
/// keeps, `modules` the ones it runs on.
fn entries_left_out<'k>(
    capabilities: &'k [KitCapability],
    offered: &'k [KitModule],
    selected: &[&KitCapability],
    modules: &DependenciesCfg,
) -> Vec<&'k str> {
    let capabilities = capabilities
        .iter()
        .filter(|c| !selected.iter().any(|s| s.name == c.name))
        .map(|c| c.register_symbol.as_str());
    let unused_modules = offered
        .iter()
        .filter(|m| !modules.0.iter().any(|dep| dep.name == m.name))
        .map(|m| m.register_symbol.as_str());
    capabilities.chain(unused_modules).collect()
}

/// A module-definition file without the exports named in `dropped`: the
/// register symbols of the capabilities left out, which would otherwise pull
/// every one of them back in.
pub(crate) fn filter_def(def: &str, dropped: &[&str]) -> String {
    let mut out = String::with_capacity(def.len());
    for line in def.lines() {
        let name = line.split_whitespace().next().unwrap_or_default();
        if dropped.contains(&name) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// The names a relinked engine keeps: every symbol the consumers leave
/// undefined that the engine defines, plus the engine's own entry points
/// (`lumen_*`, among them `lumen_engine_build_id`) and the Rust runtime's
/// (`rust_*`, `__rust*`). `registers` are left out whatever matches them: a
/// capability is in or out by its selection, never by a pattern. So is the
/// crate metadata's marker symbol, which nothing outside a compiler reads.
pub(crate) fn keep_list(
    engine: &[u8],
    consumers: &[Vec<u8>],
    registers: &[String],
) -> Result<Vec<String>, String> {
    let engine = symbols(engine).map_err(|e| format!("the toolchain's engine: {e}"))?;
    let mut needed = BTreeSet::new();
    for consumer in consumers {
        let symbols = symbols(consumer)?;
        needed.extend(symbols.undefined);
    }
    Ok(engine
        .defined
        .into_iter()
        .filter(|name| {
            needed.contains(name)
                || name.starts_with("lumen_")
                || name.starts_with("rust_")
                || name.starts_with("__rust")
        })
        .filter(|name| !registers.contains(name))
        // The crate metadata's marker, defined in the metadata object an
        // engine kit leaves off the line.
        .filter(|name| !name.starts_with("rust_metadata_"))
        .collect())
}

/// One library's dynamic symbols, split by whether it defines them. A Mach-O
/// name loses the leading underscore C symbols carry there, so the two
/// formats compare as the same names.
#[derive(Debug, Default)]
pub(crate) struct Symbols {
    /// What it exports.
    pub(crate) defined: BTreeSet<String>,
    /// What it expects another library to export.
    pub(crate) undefined: BTreeSet<String>,
}

/// Read a shared library's dynamic symbols: the dynamic symbol table of an
/// ELF file, the external symbols of a Mach-O one.
pub(crate) fn symbols(bytes: &[u8]) -> Result<Symbols, String> {
    let file = object::File::parse(bytes).map_err(|e| format!("not a library: {e}"))?;
    let macho = file.format() == BinaryFormat::MachO;
    let mut out = Symbols::default();
    let mut add = |symbol: object::Symbol<'_, '_>| {
        let Ok(name) = symbol.name() else {
            return;
        };
        let name = if macho {
            name.strip_prefix('_').unwrap_or(name)
        } else {
            name
        };
        if name.is_empty() {
            return;
        }
        if symbol.is_undefined() {
            if symbol.is_global() {
                out.undefined.insert(name.to_string());
            }
        } else if symbol.is_global() {
            out.defined.insert(name.to_string());
        }
    };
    match file.format() {
        BinaryFormat::Elf => file.dynamic_symbols().for_each(&mut add),
        BinaryFormat::MachO => file.symbols().for_each(&mut add),
        other => return Err(format!("a {other:?} library has no symbols to keep here")),
    }
    Ok(out)
}

/// The export list a relinked engine is given: a version script for the GNU
/// linkers, an exported-symbols list for ld64.
pub(crate) fn export_list(keep: &[String], macho: bool) -> String {
    let mut out = String::new();
    if macho {
        for name in keep {
            out.push('_');
            out.push_str(name);
            out.push('\n');
        }
        return out;
    }
    out.push_str("{\n  global:\n");
    for name in keep {
        out.push_str("    ");
        out.push_str(name);
        out.push_str(";\n");
    }
    out.push_str("  local:\n    *;\n};\n");
    out
}

/// Check the relinked engine exports every name the files beside it need. A
/// name the link could not find would otherwise surface as a library that
/// fails to load on someone else's machine.
fn check_exports(engine: &Path, keep: &[String]) -> Result<(), String> {
    let bytes = std::fs::read(engine).map_err(|e| format!("read {}: {e}", engine.display()))?;
    let exported = symbols(&bytes)?.defined;
    let missing: Vec<&str> = keep
        .iter()
        .filter(|name| !exported.contains(*name))
        .map(String::as_str)
        .take(3)
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    Err(format!(
        "the relinked engine does not export {}",
        missing.join(", ")
    ))
}

/// The engine build id in a library or object image: the string
/// `lumen_engine_build_id` returns, `lumen-engine <version> <source>
/// rustc:<16 hex digits>`. The constant may sit against other string data
/// with nothing between, so the id ends where its shape does rather than at
/// a terminator.
pub(crate) fn build_id_in(bytes: &[u8]) -> Option<String> {
    const HEAD: &[u8] = b"lumen-engine ";
    let mut from = 0;
    while let Some(at) = find(&bytes[from..], HEAD) {
        let start = from + at;
        if let Some(id) = parse_build_id(&bytes[start..]) {
            return Some(id);
        }
        from = start + 1;
    }
    None
}

/// The build id at the start of `bytes`, when one is there.
fn parse_build_id(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(&bytes[..bytes.len().min(512)])
        .or_else(|e| std::str::from_utf8(&bytes[..e.valid_up_to()]))
        .ok()?;
    let mut fields = text.splitn(4, ' ');
    let (head, version, source, rest) = (
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
    );
    let hash = rest.strip_prefix("rustc:")?.get(..16)?;
    let printable = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_graphic());
    if head != "lumen-engine"
        || !printable(version)
        || !printable(source)
        || !hash.chars().all(|c| c.is_ascii_hexdigit())
    {
        return None;
    }
    Some(format!("{head} {version} {source} rustc:{hash}"))
}

/// The first position of `needle` in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    let first = *needle.first()?;
    let mut at = 0;
    while let Some(i) = haystack[at..].iter().position(|b| *b == first) {
        let start = at + i;
        if haystack[start..].starts_with(needle) {
            return Some(start);
        }
        at = start + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use object::write::elf::{FileHeader, SectionHeader, Sym, Writer};
    use object::write::{Object as WriteObject, StandardSection, Symbol, SymbolSection};
    use object::{Architecture, Endianness, SymbolFlags, SymbolKind, SymbolScope, elf};

    use super::*;

    /// A shared ELF library with only a dynamic symbol table: `defined` are
    /// exported from its one section, `undefined` are left for another
    /// library. Enough of a file for the dynamic symbol reader, which is all
    /// the keep list reads.
    fn elf_library(defined: &[&str], undefined: &[&str]) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut w = Writer::new(Endianness::Little, true, &mut bytes);
        let names: Vec<_> = defined
            .iter()
            .chain(undefined)
            .map(|n| w.add_dynamic_string(n.as_bytes()))
            .collect();
        let text_name = w.add_section_name(b".text");
        w.reserve_file_header();
        w.reserve_null_section_index();
        let text = w.reserve_section_index();
        w.reserve_dynsym_section_index();
        w.reserve_dynstr_section_index();
        w.reserve_shstrtab_section_index();
        w.reserve_null_dynamic_symbol_index();
        for _ in &names {
            w.reserve_dynamic_symbol_index();
        }
        let text_offset = w.reserve(16, 16);
        w.reserve_dynsym();
        w.reserve_dynstr().expect("dynstr");
        w.reserve_shstrtab().expect("shstrtab");
        w.reserve_section_headers();

        w.write_file_header(&FileHeader {
            os_abi: elf::ELFOSABI_NONE,
            abi_version: 0,
            e_type: elf::ET_DYN,
            e_machine: elf::EM_X86_64,
            e_entry: 0,
            e_flags: Default::default(),
        })
        .expect("header");
        w.write_align(16);
        w.write(&[0xC3; 16]);
        w.write_null_dynamic_symbol();
        for (i, name) in names.iter().enumerate() {
            let st_name = w.dynamic_string_offset(Some(*name));
            w.write_dynamic_symbol(&Sym {
                section: (i < defined.len()).then_some(text.0),
                st_name,
                st_info: elf::SymbolInfo::new(elf::STB_GLOBAL, elf::STT_FUNC),
                st_other: elf::STV_DEFAULT.into(),
                st_shndx: elf::SHN_UNDEF,
                st_value: 0,
                st_size: 0,
            });
        }
        w.write_dynstr();
        w.write_shstrtab();
        w.write_null_section_header();
        let sh_name = w.section_name_offset(Some(text_name));
        w.write_section_header(&SectionHeader {
            sh_name,
            sh_type: elf::SHT_PROGBITS,
            sh_flags: elf::SHF_ALLOC | elf::SHF_EXECINSTR,
            sh_addr: 0,
            sh_offset: text_offset,
            sh_size: 16,
            sh_link: 0,
            sh_info: 0,
            sh_addralign: 16,
            sh_entsize: 0,
        });
        w.write_dynsym_section_header(0, 1);
        w.write_dynstr_section_header(0);
        w.write_shstrtab_section_header();
        bytes
    }

    /// A Mach-O object with the same split: external symbols defined in its
    /// text section, and undefined external ones. The writer adds the leading
    /// underscore a C symbol carries there, which the reader takes off.
    fn macho_image(defined: &[&str], undefined: &[&str]) -> Vec<u8> {
        let mut obj = WriteObject::new(
            object::BinaryFormat::MachO,
            Architecture::Aarch64,
            Endianness::Little,
        );
        let text = obj.section_id(StandardSection::Text);
        for name in defined {
            let offset = obj.append_section_data(text, &[0xC0, 0x03, 0x5F, 0xD6], 4);
            obj.add_symbol(Symbol {
                name: name.as_bytes().to_vec(),
                value: offset,
                size: 4,
                kind: SymbolKind::Text,
                scope: SymbolScope::Dynamic,
                weak: false,
                section: SymbolSection::Section(text),
                flags: SymbolFlags::None,
            });
        }
        for name in undefined {
            obj.add_symbol(Symbol {
                name: name.as_bytes().to_vec(),
                value: 0,
                size: 0,
                kind: SymbolKind::Text,
                scope: SymbolScope::Dynamic,
                weak: false,
                section: SymbolSection::Undefined,
                flags: SymbolFlags::None,
            });
        }
        obj.write().expect("the object writes")
    }

    fn strings(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    /// The whole keep rule on ELF: what the consumers need and the engine
    /// has, plus the engine's own entry points, never a register symbol and
    /// never a name the engine does not define.
    #[test]
    fn the_keep_list_of_an_elf_engine_is_what_its_consumers_resolve_against_it() {
        let engine = elf_library(
            &[
                "_ZN13lumen_runtime3run7run_app17h0123456789abcdefE",
                "_ZN4core3fmt5write17h0123456789abcdefE",
                "_ZN9unused_by_anybody17h0123456789abcdefE",
                "lumen_engine_build_id",
                "lumen_capability_register_os_tray",
                "__rust_alloc",
                "rust_begin_unwind",
                "rust_metadata_lumen_engine_0123456789abcdef",
            ],
            &["malloc"],
        );
        let liblumen = elf_library(
            &["lumen_app_new"],
            &[
                "_ZN13lumen_runtime3run7run_app17h0123456789abcdefE",
                "malloc",
            ],
        );
        let module = elf_library(
            &[],
            &["_ZN4core3fmt5write17h0123456789abcdefE", "__rust_alloc"],
        );
        let keep = keep_list(
            &engine,
            &[liblumen, module],
            &strings(&["lumen_capability_register_os_tray"]),
        )
        .expect("both parse");
        assert_eq!(
            keep,
            strings(&[
                "_ZN13lumen_runtime3run7run_app17h0123456789abcdefE",
                "_ZN4core3fmt5write17h0123456789abcdefE",
                "__rust_alloc",
                "lumen_engine_build_id",
                "rust_begin_unwind",
            ])
        );
    }

    /// The same rule on Mach-O, where every C symbol carries an underscore
    /// the keep list does not.
    #[test]
    fn the_keep_list_of_a_mach_o_engine_drops_the_leading_underscore() {
        let engine = macho_image(
            &[
                "lumen_engine_build_id",
                "lumen_capability_register_mcp",
                "needed_by_liblumen",
                "needed_by_nobody",
            ],
            &[],
        );
        let liblumen = macho_image(&["lumen_app_run"], &["needed_by_liblumen", "dlopen"]);
        let symbols = symbols(&liblumen).expect("parses");
        assert!(symbols.defined.contains("lumen_app_run"), "{symbols:?}");
        assert!(symbols.undefined.contains("dlopen"), "{symbols:?}");

        let keep = keep_list(
            &engine,
            &[liblumen],
            &strings(&["lumen_capability_register_mcp"]),
        )
        .expect("both parse");
        assert_eq!(
            keep,
            strings(&["lumen_engine_build_id", "needed_by_liblumen"])
        );
    }

    #[test]
    fn a_file_that_is_no_library_has_no_keep_list() {
        let error = keep_list(b"not a library", &[], &[]).expect_err("nothing to read");
        assert!(error.contains("engine"), "{error}");
    }

    #[test]
    fn the_export_list_is_written_in_each_linker_s_format() {
        let keep = strings(&["lumen_engine_build_id", "_ZN1a1bE"]);
        assert_eq!(
            export_list(&keep, false),
            "{\n  global:\n    lumen_engine_build_id;\n    _ZN1a1bE;\n  local:\n    *;\n};\n"
        );
        assert_eq!(
            export_list(&keep, true),
            "_lumen_engine_build_id\n__ZN1a1bE\n"
        );
    }

    #[test]
    fn a_replay_drops_the_entries_of_what_the_app_does_without() {
        use lumen_modules::link_kit::KitSelect;
        use lumen_modules::{DepCfg, ModuleSource};

        let capability = |name: &str| KitCapability {
            name: name.to_string(),
            register_symbol: lumen_capability::register_symbol(name),
            select: KitSelect::Always,
        };
        let capabilities = [capability("os-tray"), capability("mcp")];
        let offered = [
            KitModule::new("lumen-candela"),
            KitModule::new("lumen-candela-dev"),
            KitModule::new("lumen-lua"),
        ];
        let runs_on = DependenciesCfg(vec![DepCfg {
            name: "lumen-lua".to_string(),
            source: ModuleSource::Bundled,
            config: toml::Table::new(),
            tags: Vec::new(),
        }]);

        assert_eq!(
            entries_left_out(&capabilities, &offered, &[&capabilities[1]], &runs_on),
            [
                lumen_capability::register_symbol("os-tray").as_str(),
                "lumen_module_register_lumen_candela",
                "lumen_module_register_lumen_candela_dev",
            ],
            "the module the app runs on and the capability it keeps stay exported"
        );
    }

    #[test]
    fn a_module_definition_file_loses_the_register_symbols_left_out() {
        let def = "LIBRARY\nEXPORTS\n    lumen_app_new\n    lumen_capability_register_os_tray\n    \
                   lumen_capability_register_mcp\n";
        assert_eq!(
            filter_def(def, &["lumen_capability_register_os_tray"]),
            "LIBRARY\nEXPORTS\n    lumen_app_new\n    lumen_capability_register_mcp\n"
        );
    }

    #[test]
    fn the_build_id_is_read_out_of_an_image_by_its_shape() {
        let id = "lumen-engine 0.0.9 git:v0.0.9 rustc:0123456789abcdef";
        let mut image = b"\x00\x01lumen-engine is a word\x00".to_vec();
        image.extend_from_slice(id.as_bytes());
        image.extend_from_slice(b"next string with no terminator between");
        assert_eq!(build_id_in(&image).as_deref(), Some(id));
        assert_eq!(build_id_in(b"lumen-engine 0.0.9 nogit rustc:xyz"), None);
        assert_eq!(build_id_in(b"nothing here"), None);
    }
}
