//! The loader side: dlopen plugin cdylibs, verify their descriptors, drive
//! their hooks. Compiled only under the `host` feature; lumenc and
//! lumen-runtime are the consumers, never plugin authors.

use std::ffi::CStr;
use std::path::{Path, PathBuf};

use libloading::Library;

use crate::abi::{self, Buf, Desc};
use crate::resolve::LockFile;
use crate::{Ctx, Finding, LayoutIR, Output, PluginCfg, PluginSource, codec};

/// Which source text a transform call carries.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Markup,
    Css,
}

/// A plugin failure. Every rendering names the plugin, so a compile error
/// always says which plugin caused it.
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("plugin '{name}': no file at any of: {}", probed.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "))]
    NotFound { name: String, probed: Vec<PathBuf> },
    #[error("{0}")]
    Resolve(String),
    #[error("plugin '{name}': failed to open {path}: {message}")]
    Open {
        name: String,
        path: PathBuf,
        message: String,
    },
    #[error(
        "plugin '{name}': {path} exports no lumenc_plugin_v1 entry; is it a lumenc compiler plugin?"
    )]
    MissingEntry { name: String, path: PathBuf },
    #[error("plugin '{name}': the entry returned a null descriptor")]
    NullDescriptor { name: String },
    #[error(
        "plugin '{name}': built for plugin ABI {got}, this lumenc speaks {want}; rebuild the plugin against the matching Lumen tag"
    )]
    AbiMismatch { name: String, want: u32, got: u32 },
    #[error(
        "plugin '{name}': built against IR format {got}, this lumenc uses {want}; rebuild the plugin against the matching Lumen tag"
    )]
    IrVersionMismatch { name: String, want: u16, got: u16 },
    #[error(
        "plugin '{declared}': the library reports itself as '{reported}'; fix the `name` in lumen.toml or the `path`"
    )]
    NameMismatch { declared: String, reported: String },
    #[error("plugin '{name}': bad descriptor: {reason}")]
    BadDescriptor { name: String, reason: String },
    #[error("plugin '{plugin}': {hook} failed: {message}")]
    Hook {
        plugin: String,
        hook: &'static str,
        message: String,
    },
    #[error("plugin '{plugin}': {hook} panicked: {message}")]
    Panicked {
        plugin: String,
        hook: &'static str,
        message: String,
    },
    #[error("plugin '{plugin}': {hook} returned undecodable data: {message}")]
    Codec {
        plugin: String,
        hook: &'static str,
        message: String,
    },
    #[error("plugin '{plugin}': emit output '{path}': {reason}")]
    BadOutput {
        plugin: String,
        path: String,
        reason: String,
    },
    #[error("plugin '{plugin}': writing {path}: {source}")]
    Write {
        plugin: String,
        path: PathBuf,
        source: std::io::Error,
    },
}

#[derive(Debug)]
struct Loaded {
    name: String,
    /// The plugin's own `config` table, re-serialized for [`Ctx`].
    config_toml: String,
    /// Kept open for the process lifetime; `desc` points into it.
    _lib: Library,
    desc: *const Desc,
}

// `desc` points at a static inside the library, which stays open as long as
// `_lib` lives in the same struct; `Desc` itself is Sync.
unsafe impl Send for Loaded {}
unsafe impl Sync for Loaded {}

impl Loaded {
    fn desc(&self) -> &Desc {
        unsafe { &*self.desc }
    }
}

/// The loaded plugin chain of one app, in `lumen.toml` declaration order.
/// Libraries are dlopen'd once and held for the process lifetime; a hot
/// reload pays only the hook calls.
#[derive(Debug)]
pub struct PluginSet {
    plugins: Vec<Loaded>,
    app_dir: PathBuf,
    check_only: bool,
}

/// One hook's outcome at the byte level.
enum HookOut {
    Unchanged,
    Bytes(Vec<u8>),
}

impl PluginSet {
    /// Load every declared plugin, in order, verifying each descriptor.
    /// `version` sources resolve through the plugin cache and pin into
    /// `lumen.lock` (written back here when it changed).
    pub fn load(app_dir: &Path, cfgs: &[PluginCfg]) -> Result<Self, PluginError> {
        let mut lock = if cfgs
            .iter()
            .any(|c| matches!(c.source, PluginSource::Version(_)))
        {
            Some(LockFile::read(app_dir).map_err(PluginError::Resolve)?)
        } else {
            None
        };
        let mut plugins = Vec::with_capacity(cfgs.len());
        for cfg in cfgs {
            let path = match &cfg.source {
                PluginSource::Path(p) => {
                    crate::resolve_plugin_path(app_dir, p).map_err(|probed| {
                        PluginError::NotFound {
                            name: cfg.name.clone(),
                            probed,
                        }
                    })?
                }
                PluginSource::Version(req) => crate::resolve::resolve_version_source(
                    &cfg.name,
                    req,
                    lock.as_mut()
                        .expect("lock read when a version source exists"),
                )
                .map_err(PluginError::Resolve)?,
            };
            let lib = unsafe { Library::new(&path) }.map_err(|e| PluginError::Open {
                name: cfg.name.clone(),
                path: path.clone(),
                // libloading's Display is a bare "dlopen failed"; the OS
                // reason (missing dependency, wrong architecture, not a
                // library) sits in the source chain, and it is the only
                // actionable part.
                message: error_chain(&e),
            })?;
            let entry: libloading::Symbol<unsafe extern "C" fn() -> *const Desc> =
                unsafe { lib.get(abi::ENTRY) }.map_err(|_| PluginError::MissingEntry {
                    name: cfg.name.clone(),
                    path: path.clone(),
                })?;
            let desc = non_null(&cfg.name, unsafe { entry() })?;
            verify(&cfg.name, desc)?;
            // A `toml::Table` always re-serializes: keys are strings and the
            // serializer orders values ahead of tables itself.
            let config_toml =
                toml::to_string(&cfg.config).expect("a toml::Table always re-serializes");
            plugins.push(Loaded {
                name: cfg.name.clone(),
                config_toml,
                _lib: lib,
                desc,
            });
        }
        if let Some(lock) = &lock {
            lock.store().map_err(PluginError::Resolve)?;
        }
        Ok(PluginSet {
            plugins,
            // Canonicalized so `Ctx::app_dir` (and the emit root) reads the
            // same regardless of the cwd lumenc ran from; kept as given when
            // canonicalization fails (the load above already used it).
            app_dir: std::fs::canonicalize(app_dir).unwrap_or_else(|_| app_dir.to_path_buf()),
            check_only: false,
        })
    }

    /// Mark the set as running under `lumenc check`: hooks still run, emit
    /// outputs are discarded.
    pub fn check_only(mut self, yes: bool) -> Self {
        self.check_only = yes;
        self
    }

    fn ctx(&self, plugin: &Loaded, entry: &Path, file: &Path) -> Vec<u8> {
        let ctx = Ctx::new(
            self.app_dir.clone(),
            entry.to_path_buf(),
            file.to_path_buf(),
            self.check_only,
            plugin.config_toml.clone(),
        );
        codec::encode(&ctx).expect("Ctx always encodes")
    }

    /// Run one source text through every plugin's transform, in order.
    pub fn transform_source(
        &self,
        kind: SourceKind,
        src: String,
        entry: &Path,
        file: &Path,
    ) -> Result<String, PluginError> {
        if self.plugins.is_empty() {
            return Ok(src);
        }
        let (pick, hook_name): (fn(&Desc) -> Option<abi::HookFn>, _) = match kind {
            SourceKind::Markup => (|d: &Desc| d.transform_markup, "transform_markup"),
            SourceKind::Css => (|d: &Desc| d.transform_css, "transform_css"),
        };
        let mut current = src;
        for plugin in &self.plugins {
            let Some(hook) = pick(plugin.desc()) else {
                continue;
            };
            let ctx = self.ctx(plugin, entry, file);
            match call(plugin, hook, hook_name, current.as_bytes(), &ctx)? {
                HookOut::Unchanged => {}
                HookOut::Bytes(bytes) => {
                    current = String::from_utf8(bytes).map_err(|e| PluginError::Codec {
                        plugin: plugin.name.clone(),
                        hook: hook_name,
                        message: e.to_string(),
                    })?;
                }
            }
        }
        Ok(current)
    }

    /// Run the tree through every plugin's IR transform, in order. The tree
    /// is encoded once, threaded through the chain as bytes, and decoded
    /// once at the end; if no plugin changed it, it is not touched at all.
    pub fn transform_ir(&self, ir: &mut LayoutIR, entry: &Path) -> Result<(), PluginError> {
        if self.plugins.is_empty() {
            return Ok(());
        }
        let mut bytes: Option<Vec<u8>> = None;
        let mut changed = false;
        for plugin in &self.plugins {
            let Some(hook) = plugin.desc().transform_ir else {
                continue;
            };
            if bytes.is_none() {
                // Same contract as `Ctx` above and the artifact writer: a
                // `LayoutIR` always bincode-encodes; failing here means the
                // IR type itself broke, not the plugin.
                bytes = Some(codec::encode(&*ir).expect("LayoutIR always encodes"));
            }
            let ctx = self.ctx(plugin, entry, entry);
            let input = bytes.as_deref().expect("encoded above");
            match call(plugin, hook, "transform_ir", input, &ctx)? {
                HookOut::Unchanged => {}
                HookOut::Bytes(out) => {
                    bytes = Some(out);
                    changed = true;
                }
            }
        }
        if changed {
            let bytes = bytes.expect("changed implies encoded");
            *ir = codec::decode(&bytes).map_err(|e| PluginError::Codec {
                plugin: self
                    .plugins
                    .last()
                    .map(|p| p.name.clone())
                    .unwrap_or_default(),
                hook: "transform_ir",
                message: e,
            })?;
        }
        Ok(())
    }

    /// Run every plugin's lint and emit hooks over one shared encoding of
    /// the cascaded tree. Emit outputs are written under
    /// `.lumen/generated/<plugin>/` (discarded under check). Returns the
    /// findings, each paired with its plugin's name.
    pub fn finish(
        &self,
        ir: &LayoutIR,
        entry: &Path,
    ) -> Result<Vec<(String, Finding)>, PluginError> {
        if self.plugins.is_empty() {
            return Ok(Vec::new());
        }
        let mut bytes: Option<Vec<u8>> = None;
        let mut findings = Vec::new();
        let mut emitted: Vec<(String, Vec<Output>)> = Vec::new();
        for plugin in &self.plugins {
            let desc = plugin.desc();
            if desc.lint.is_none() && desc.emit.is_none() {
                continue;
            }
            if bytes.is_none() {
                bytes = Some(codec::encode(ir).expect("LayoutIR always encodes"));
            }
            let input = bytes.as_deref().expect("encoded above");
            let ctx = self.ctx(plugin, entry, entry);
            if let Some(hook) = desc.lint {
                if let HookOut::Bytes(out) = call(plugin, hook, "lint", input, &ctx)? {
                    let batch: Vec<Finding> =
                        codec::decode(&out).map_err(|e| PluginError::Codec {
                            plugin: plugin.name.clone(),
                            hook: "lint",
                            message: e,
                        })?;
                    findings.extend(batch.into_iter().map(|f| (plugin.name.clone(), f)));
                }
            }
            if let Some(hook) = desc.emit {
                if let HookOut::Bytes(out) = call(plugin, hook, "emit", input, &ctx)? {
                    let outputs: Vec<Output> =
                        codec::decode(&out).map_err(|e| PluginError::Codec {
                            plugin: plugin.name.clone(),
                            hook: "emit",
                            message: e,
                        })?;
                    // Accumulated per NAME and written once after the chain:
                    // an app may declare one plugin several times (different
                    // configs), and per-call writes would let a later entry's
                    // directory reset destroy an earlier entry's files.
                    match emitted.iter_mut().find(|(n, _)| *n == plugin.name) {
                        Some((_, all)) => all.extend(outputs),
                        None => emitted.push((plugin.name.clone(), outputs)),
                    }
                }
            }
        }
        if !self.check_only {
            for (name, outputs) in &emitted {
                self.write_outputs(name, outputs)?;
            }
        }
        Ok(findings)
    }

    fn write_outputs(&self, plugin: &str, outputs: &[Output]) -> Result<(), PluginError> {
        let root = self.app_dir.join(".lumen").join("generated").join(plugin);
        let mut seen = std::collections::HashSet::new();
        for out in outputs {
            let rel = Path::new(&out.path);
            if rel.as_os_str().is_empty() || rel.is_absolute() {
                return Err(PluginError::BadOutput {
                    plugin: plugin.to_string(),
                    path: out.path.clone(),
                    reason: "path must be relative and non-empty".to_string(),
                });
            }
            if rel
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
            {
                return Err(PluginError::BadOutput {
                    plugin: plugin.to_string(),
                    path: out.path.clone(),
                    reason: "path must not contain `..` or a root".to_string(),
                });
            }
            if !seen.insert(out.path.clone()) {
                return Err(PluginError::BadOutput {
                    plugin: plugin.to_string(),
                    path: out.path.clone(),
                    reason: "duplicate output path".to_string(),
                });
            }
        }
        // Recreate the plugin's directory so stale outputs from a previous
        // compile cannot linger beside fresh ones.
        if root.exists() {
            std::fs::remove_dir_all(&root).map_err(|source| PluginError::Write {
                plugin: plugin.to_string(),
                path: root.clone(),
                source,
            })?;
        }
        if outputs.is_empty() {
            return Ok(());
        }
        for out in outputs {
            let dest = root.join(&out.path);
            // `out.path` is relative and non-empty (validated above), so the
            // destination always has a parent under the per-plugin root.
            let parent = dest.parent().expect("dest sits under the plugin root");
            std::fs::create_dir_all(parent).map_err(|source| PluginError::Write {
                plugin: plugin.to_string(),
                path: parent.to_path_buf(),
                source,
            })?;
            std::fs::write(&dest, &out.bytes).map_err(|source| PluginError::Write {
                plugin: plugin.to_string(),
                path: dest.clone(),
                source,
            })?;
        }
        Ok(())
    }

    /// Render plugin findings in the same shape as the built-in lint lines:
    /// severity, anchor, `[<plugin>/<rule>]`, message, optional hint. The
    /// format string mirrors `lumen_ir::layout_ir::LintFinding::render`
    /// (which cannot be reused: its kind slot is a closed enum, this one is
    /// a plugin/rule pair); keep the two in sync.
    pub fn render_findings(findings: &[(String, Finding)], entry: &Path) -> Vec<String> {
        findings
            .iter()
            .map(|(plugin, f)| {
                let file = f.file.as_deref().unwrap_or(entry);
                let mut line = format!(
                    "{sev:<5} {file}:{l}:{c}  [{plugin}/{rule}] {msg}",
                    sev = f.severity.label(),
                    file = file.display(),
                    l = f.line,
                    c = f.col,
                    plugin = plugin,
                    rule = f.rule,
                    msg = f.message,
                );
                if let Some(s) = &f.suggest {
                    line.push_str(&format!("\n      hint: replace with `{s}`"));
                }
                line
            })
            .collect()
    }
}

/// The handshake: refuse anything about the descriptor that would make the
/// byte payloads or the pointers untrustworthy. The first two fields sit at
/// frozen offsets and are read through the raw pointer before a `&Desc` for
/// the whole struct exists, so a truncated or foreign descriptor is refused
/// without ever forming a reference past its end.
///
/// # Safety (internal)
/// `desc` is non-null and points at at least 8 readable bytes; the caller
/// checked null, and any exporter of the entry symbol provides at least the
/// frozen prefix.
fn verify(declared: &str, desc: *const Desc) -> Result<(), PluginError> {
    let name = declared.to_string();
    let abi_version = unsafe { (desc as *const u32).read() };
    let struct_size = unsafe { (desc as *const u32).add(1).read() };
    if abi_version != abi::ABI_VERSION {
        return Err(PluginError::AbiMismatch {
            name,
            want: abi::ABI_VERSION,
            got: abi_version,
        });
    }
    if (struct_size as usize) < std::mem::size_of::<Desc>() {
        return Err(PluginError::BadDescriptor {
            name,
            reason: format!(
                "descriptor is {struct_size} bytes, expected at least {}",
                std::mem::size_of::<Desc>()
            ),
        });
    }
    let desc: &Desc = unsafe { &*desc };
    if desc.flags & abi::FLAG_PANIC_ABORT != 0 {
        return Err(PluginError::BadDescriptor {
            name,
            reason: "built with panic = \"abort\"; the panic-to-error contract needs \
                     unwinding, rebuild with the default panic = \"unwind\""
                .to_string(),
        });
    }
    if desc.ir_format_version != lumen_ir::artifact::FORMAT_VERSION {
        return Err(PluginError::IrVersionMismatch {
            name,
            want: lumen_ir::artifact::FORMAT_VERSION,
            got: desc.ir_format_version,
        });
    }
    let c_str = |ptr: *const std::os::raw::c_char, what: &str| -> Result<String, PluginError> {
        if ptr.is_null() {
            return Err(PluginError::BadDescriptor {
                name: declared.to_string(),
                reason: format!("null {what}"),
            });
        }
        unsafe { CStr::from_ptr(ptr) }
            .to_str()
            .map(str::to_string)
            .map_err(|_| PluginError::BadDescriptor {
                name: declared.to_string(),
                reason: format!("{what} is not UTF-8"),
            })
    };
    let reported = c_str(desc.name, "name")?;
    c_str(desc.version, "version")?;
    if reported.is_empty() {
        return Err(PluginError::BadDescriptor {
            name,
            reason: "empty name".to_string(),
        });
    }
    if reported != declared {
        return Err(PluginError::NameMismatch {
            declared: declared.to_string(),
            reported,
        });
    }
    if desc.free.is_none() {
        return Err(PluginError::BadDescriptor {
            name,
            reason: "no free function".to_string(),
        });
    }
    Ok(())
}

/// Refuse a null descriptor pointer from the entry symbol.
fn non_null(name: &str, desc: *const Desc) -> Result<*const Desc, PluginError> {
    if desc.is_null() {
        return Err(PluginError::NullDescriptor {
            name: name.to_string(),
        });
    }
    Ok(desc)
}

/// Join an error with its source chain, most specific last.
fn error_chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut cur = e.source();
    while let Some(s) = cur {
        out.push_str(": ");
        out.push_str(&s.to_string());
        cur = s.source();
    }
    out
}

/// One FFI hook call: borrowed input in, plugin-owned buffer out, freed here
/// through the plugin's own `free` after copying.
fn call(
    plugin: &Loaded,
    hook: abi::HookFn,
    hook_name: &'static str,
    input: &[u8],
    ctx: &[u8],
) -> Result<HookOut, PluginError> {
    let mut out = Buf::empty();
    let status = unsafe {
        hook(
            input.as_ptr(),
            input.len(),
            ctx.as_ptr(),
            ctx.len(),
            &mut out,
        )
    };
    let bytes = if out.ptr.is_null() {
        Vec::new()
    } else {
        let copied = unsafe { std::slice::from_raw_parts(out.ptr, out.len) }.to_vec();
        let free = plugin.desc().free.expect("verified at load");
        unsafe { free(out.ptr, out.len, out.cap) };
        copied
    };
    match status {
        abi::OK => Ok(HookOut::Bytes(bytes)),
        abi::UNCHANGED => Ok(HookOut::Unchanged),
        abi::ERR => Err(PluginError::Hook {
            plugin: plugin.name.clone(),
            hook: hook_name,
            message: String::from_utf8_lossy(&bytes).into_owned(),
        }),
        abi::PANICKED => Err(PluginError::Panicked {
            plugin: plugin.name.clone(),
            hook: hook_name,
            message: String::from_utf8_lossy(&bytes).into_owned(),
        }),
        other => Err(PluginError::BadDescriptor {
            name: plugin.name.clone(),
            reason: format!("{hook_name} returned unknown status {other}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abi::ABI_VERSION;

    fn good_desc() -> Desc {
        Desc {
            abi_version: ABI_VERSION,
            struct_size: std::mem::size_of::<Desc>() as u32,
            ir_format_version: lumen_ir::artifact::FORMAT_VERSION,
            flags: 0,
            name: c"demo".as_ptr(),
            version: c"0.1.0".as_ptr(),
            transform_markup: None,
            transform_css: None,
            transform_ir: None,
            lint: None,
            emit: None,
            free: Some(crate::export::free_buf),
        }
    }

    #[test]
    fn good_descriptor_verifies() {
        verify("demo", &good_desc() as *const Desc).unwrap();
    }

    #[test]
    fn wrong_abi_version_is_refused() {
        let mut d = good_desc();
        d.abi_version = ABI_VERSION + 1;
        let err = verify("demo", &d as *const Desc).unwrap_err().to_string();
        assert!(err.contains("plugin 'demo'"), "{err}");
        assert!(err.contains("ABI"), "{err}");
    }

    #[test]
    fn short_struct_is_refused() {
        let mut d = good_desc();
        d.struct_size = 8;
        let err = verify("demo", &d as *const Desc).unwrap_err().to_string();
        assert!(err.contains("descriptor is 8 bytes"), "{err}");
    }

    #[test]
    fn wrong_ir_version_is_refused() {
        let mut d = good_desc();
        d.ir_format_version = 1;
        let err = verify("demo", &d as *const Desc).unwrap_err().to_string();
        assert!(err.contains("IR format"), "{err}");
        assert!(err.contains("matching Lumen tag"), "{err}");
    }

    #[test]
    fn null_name_is_refused() {
        let mut d = good_desc();
        d.name = std::ptr::null();
        let err = verify("demo", &d as *const Desc).unwrap_err().to_string();
        assert!(err.contains("null name"), "{err}");
    }

    #[test]
    fn name_mismatch_names_both() {
        let err = verify("other", &good_desc() as *const Desc)
            .unwrap_err()
            .to_string();
        assert!(err.contains("'other'"), "{err}");
        assert!(err.contains("'demo'"), "{err}");
    }

    #[test]
    fn missing_free_is_refused() {
        let mut d = good_desc();
        d.free = None;
        let err = verify("demo", &d as *const Desc).unwrap_err().to_string();
        assert!(err.contains("no free function"), "{err}");
    }

    #[test]
    fn emit_paths_are_validated() {
        let dir = std::env::temp_dir().join("lumenc-plugin-emit-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let set = PluginSet {
            plugins: Vec::new(),
            app_dir: dir.clone(),
            check_only: false,
        };
        let bad = |path: &str| Output {
            path: path.to_string(),
            bytes: Vec::new(),
        };
        for path in ["/abs/x", "../up", ""] {
            let err = set.write_outputs("demo", &[bad(path)]).unwrap_err();
            assert!(matches!(err, PluginError::BadOutput { .. }), "{path}");
        }
        let err = set
            .write_outputs("demo", &[bad("a.txt"), bad("a.txt")])
            .unwrap_err();
        assert!(err.to_string().contains("duplicate"), "{err}");

        set.write_outputs(
            "demo",
            &[Output {
                path: "sub/ok.txt".to_string(),
                bytes: b"x".to_vec(),
            }],
        )
        .unwrap();
        let written = dir.join(".lumen/generated/demo/sub/ok.txt");
        assert_eq!(std::fs::read(&written).unwrap(), b"x");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_null_descriptor_is_refused() {
        let err = non_null("demo", std::ptr::null()).unwrap_err();
        assert!(matches!(err, PluginError::NullDescriptor { .. }), "{err}");
    }

    #[test]
    fn a_panic_abort_plugin_is_refused() {
        let mut d = good_desc();
        d.flags = abi::FLAG_PANIC_ABORT;
        let err = verify("demo", &d as *const Desc).unwrap_err().to_string();
        assert!(err.contains("panic = \"abort\""), "{err}");
    }

    #[test]
    fn a_non_utf8_name_is_refused() {
        let mut d = good_desc();
        d.name = c"\xff\xfe".as_ptr();
        let err = verify("demo", &d as *const Desc).unwrap_err().to_string();
        assert!(err.contains("not UTF-8"), "{err}");
    }

    #[test]
    fn an_empty_reported_name_is_refused() {
        let mut d = good_desc();
        d.name = c"".as_ptr();
        let err = verify("", &d as *const Desc).unwrap_err().to_string();
        assert!(err.contains("empty name"), "{err}");
    }

    #[test]
    fn error_chains_join_every_source() {
        #[derive(Debug)]
        struct Outer(std::io::Error);
        impl std::fmt::Display for Outer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "outer")
            }
        }
        impl std::error::Error for Outer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }
        let joined = error_chain(&Outer(std::io::Error::other("inner reason")));
        assert_eq!(joined, "outer: inner reason");
    }

    #[test]
    fn rendered_findings_carry_the_hint_line() {
        let findings = vec![(
            "demo".to_string(),
            Finding {
                rule: "r".to_string(),
                severity: crate::Severity::Warn,
                message: "m".to_string(),
                file: None,
                line: 2,
                col: 3,
                suggest: Some("use this".to_string()),
            },
        )];
        let lines = PluginSet::render_findings(&findings, Path::new("main.lmn"));
        let line = &lines[0];
        assert!(line.contains("hint: replace with `use this`"), "{line}");
    }

    #[test]
    fn a_text_file_fails_to_open_with_the_os_reason() {
        let dir = test_dir("not-a-lib");
        let fake = dir.join("libfake.so");
        std::fs::write(&fake, b"this is not an elf").unwrap();
        let doc: toml::Table = toml::from_str(&format!(
            "[[plugins]]\nname = \"fake\"\npath = '{}'\n",
            fake.display()
        ))
        .unwrap();
        let err = PluginSet::load(&dir, &crate::PluginCfg::from_document(&doc).unwrap())
            .unwrap_err()
            .to_string();
        assert!(err.contains("failed to open"), "{err}");
        // The libloading Display alone is "dlopen failed"; the chain carries
        // the loader's reason, which is the actionable part.
        assert!(
            err.len() > "plugin 'fake': failed to open : dlopen failed".len() + 10,
            "{err}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_library_without_the_entry_symbol_is_refused() {
        // Any real shared library that is not a lumenc plugin serves; libc
        // is what the test binary itself is already linked against.
        let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
        let libc = maps
            .lines()
            .filter_map(|l| l.split_whitespace().last())
            .find(|p| {
                p.rsplit('/')
                    .next()
                    .is_some_and(|f| f.starts_with("libc.so"))
            })
            .expect("test binary maps libc");
        let dir = test_dir("no-entry");
        let doc: toml::Table =
            toml::from_str(&format!("[[plugins]]\nname = \"libc\"\npath = '{libc}'\n")).unwrap();
        let err = PluginSet::load(&dir, &crate::PluginCfg::from_document(&doc).unwrap())
            .unwrap_err()
            .to_string();
        assert!(err.contains("exports no lumenc_plugin_v1 entry"), "{err}");
    }

    // -- hand-built descriptor harness ------------------------------------
    //
    // A `Loaded` over the test binary's own handle (`Library::this`) and a
    // static descriptor, so the chain methods can be driven against hook
    // behaviors no well-formed plugin produces: absent hooks, garbage
    // payloads, unknown status codes.

    #[cfg(unix)]
    fn set_with(desc: &'static Desc, app_dir: &Path) -> PluginSet {
        let lib = Library::from(libloading::os::unix::Library::this());
        PluginSet {
            plugins: vec![Loaded {
                name: "harness".to_string(),
                config_toml: String::new(),
                _lib: lib,
                desc,
            }],
            app_dir: app_dir.to_path_buf(),
            check_only: false,
        }
    }

    fn test_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lumenc-plugin-host-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    unsafe extern "C" fn garbage_hook(
        _input: *const u8,
        _input_len: usize,
        _ctx: *const u8,
        _ctx_len: usize,
        out: *mut Buf,
    ) -> i32 {
        let bytes = std::mem::ManuallyDrop::new(vec![0xffu8, 0xfe, 0x00, 0x9d]);
        unsafe {
            (*out).ptr = bytes.as_ptr() as *mut u8;
            (*out).len = bytes.len();
            (*out).cap = bytes.capacity();
        }
        abi::OK
    }

    unsafe extern "C" fn unchanged_hook(
        _input: *const u8,
        _input_len: usize,
        _ctx: *const u8,
        _ctx_len: usize,
        _out: *mut Buf,
    ) -> i32 {
        abi::UNCHANGED
    }

    /// Lint answers with a valid empty finding list; emit answers with
    /// garbage, so the emit decode arm is reachable past a healthy lint.
    unsafe extern "C" fn lint_ok_emit_garbage_hook(
        input: *const u8,
        input_len: usize,
        ctx: *const u8,
        ctx_len: usize,
        out: *mut Buf,
    ) -> i32 {
        let is_lint_shape = {
            // Both hooks receive the same IR payload; differentiate by a
            // thread-local call counter instead: first call is lint.
            thread_local! {
                static CALLS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
            }
            CALLS.with(|c| {
                let n = c.get();
                c.set(n + 1);
                n == 0
            })
        };
        if is_lint_shape {
            let findings: Vec<Finding> = Vec::new();
            let bytes = std::mem::ManuallyDrop::new(codec::encode(&findings).unwrap());
            unsafe {
                (*out).ptr = bytes.as_ptr() as *mut u8;
                (*out).len = bytes.len();
                (*out).cap = bytes.capacity();
            }
            abi::OK
        } else {
            unsafe { garbage_hook(input, input_len, ctx, ctx_len, out) }
        }
    }

    unsafe extern "C" fn weird_status_hook(
        _input: *const u8,
        _input_len: usize,
        _ctx: *const u8,
        _ctx_len: usize,
        _out: *mut Buf,
    ) -> i32 {
        99
    }

    unsafe extern "C" fn leak_free(_ptr: *mut u8, _len: usize, _cap: usize) {}

    #[cfg(unix)]
    fn harness_desc(hook: abi::HookFn) -> Desc {
        Desc {
            abi_version: abi::ABI_VERSION,
            struct_size: std::mem::size_of::<Desc>() as u32,
            ir_format_version: lumen_ir::artifact::FORMAT_VERSION,
            flags: 0,
            name: c"harness".as_ptr(),
            version: c"0.0.0".as_ptr(),
            transform_markup: Some(hook),
            transform_css: Some(hook),
            transform_ir: Some(hook),
            lint: Some(hook),
            emit: Some(hook),
            free: Some(leak_free),
        }
    }

    #[cfg(unix)]
    #[test]
    fn absent_hooks_are_skipped_not_called() {
        static NO_HOOKS: Desc = Desc {
            abi_version: abi::ABI_VERSION,
            struct_size: std::mem::size_of::<Desc>() as u32,
            ir_format_version: 0, // never verified: built by hand, not loaded
            flags: 0,
            name: c"harness".as_ptr(),
            version: c"0.0.0".as_ptr(),
            transform_markup: None,
            transform_css: None,
            transform_ir: None,
            lint: None,
            emit: None,
            free: Some(leak_free),
        };
        let dir = test_dir("absent-hooks");
        let set = set_with(&NO_HOOKS, &dir);
        let entry = dir.join("main.lmn");
        let out = set
            .transform_source(SourceKind::Markup, "keep".to_string(), &entry, &entry)
            .unwrap();
        assert_eq!(out, "keep");
        let mut ir = LayoutIR::default();
        set.transform_ir(&mut ir, &entry).unwrap();
        assert!(set.finish(&ir, &entry).unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn garbage_hook_payloads_surface_as_codec_errors() {
        static GARBAGE: std::sync::OnceLock<Desc> = std::sync::OnceLock::new();
        let desc = GARBAGE.get_or_init(|| harness_desc(garbage_hook));
        // The OnceLock cell lives for the process; the set borrows it.
        let desc: &'static Desc = unsafe { &*(desc as *const Desc) };
        let dir = test_dir("garbage");
        let set = set_with(desc, &dir);
        let entry = dir.join("main.lmn");

        let err = set
            .transform_source(SourceKind::Markup, "x".to_string(), &entry, &entry)
            .unwrap_err();
        assert!(
            matches!(
                err,
                PluginError::Codec {
                    hook: "transform_markup",
                    ..
                }
            ),
            "{err}"
        );

        let err = set
            .transform_ir(&mut LayoutIR::default(), &entry)
            .unwrap_err();
        assert!(
            matches!(
                err,
                PluginError::Codec {
                    hook: "transform_ir",
                    ..
                }
            ),
            "{err}"
        );

        let err = set.finish(&LayoutIR::default(), &entry).unwrap_err();
        assert!(
            matches!(err, PluginError::Codec { hook: "lint", .. }),
            "{err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_unchanged_ir_hook_leaves_the_tree_alone() {
        static UNCHANGED: std::sync::OnceLock<Desc> = std::sync::OnceLock::new();
        let desc = UNCHANGED.get_or_init(|| harness_desc(unchanged_hook));
        let desc: &'static Desc = unsafe { &*(desc as *const Desc) };
        let dir = test_dir("unchanged-ir");
        let set = set_with(desc, &dir);
        let mut ir = LayoutIR::default();
        let before = ir.root.tag.clone();
        set.transform_ir(&mut ir, &dir.join("main.lmn")).unwrap();
        assert_eq!(ir.root.tag, before);
    }

    #[cfg(unix)]
    #[test]
    fn unchanged_lint_and_emit_answers_produce_nothing() {
        static UNCHANGED_ALL: std::sync::OnceLock<Desc> = std::sync::OnceLock::new();
        let desc = UNCHANGED_ALL.get_or_init(|| harness_desc(unchanged_hook));
        let desc: &'static Desc = unsafe { &*(desc as *const Desc) };
        let dir = test_dir("unchanged-finish");
        let set = set_with(desc, &dir);
        let findings = set
            .finish(&LayoutIR::default(), &dir.join("main.lmn"))
            .unwrap();
        assert!(findings.is_empty());
        assert!(!dir.join(".lumen").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_plugin_with_only_one_of_lint_and_emit_runs_the_one_it_has() {
        static LINT_ONLY: std::sync::OnceLock<Desc> = std::sync::OnceLock::new();
        let lint_only = LINT_ONLY.get_or_init(|| {
            let mut d = harness_desc(unchanged_hook);
            d.emit = None;
            d
        });
        static EMIT_ONLY: std::sync::OnceLock<Desc> = std::sync::OnceLock::new();
        let emit_only = EMIT_ONLY.get_or_init(|| {
            let mut d = harness_desc(unchanged_hook);
            d.lint = None;
            d
        });
        for desc in [lint_only, emit_only] {
            let desc: &'static Desc = unsafe { &*(desc as *const Desc) };
            let dir = test_dir("mixed-hooks");
            let set = set_with(desc, &dir);
            assert!(
                set.finish(&LayoutIR::default(), &dir.join("main.lmn"))
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_garbage_emit_payload_past_a_healthy_lint_names_the_hook() {
        static MIXED: std::sync::OnceLock<Desc> = std::sync::OnceLock::new();
        let desc = MIXED.get_or_init(|| harness_desc(lint_ok_emit_garbage_hook));
        let desc: &'static Desc = unsafe { &*(desc as *const Desc) };
        let dir = test_dir("emit-garbage");
        let set = set_with(desc, &dir);
        let err = set
            .finish(&LayoutIR::default(), &dir.join("main.lmn"))
            .unwrap_err();
        assert!(
            matches!(err, PluginError::Codec { hook: "emit", .. }),
            "{err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_unknown_status_code_is_a_bad_descriptor() {
        static WEIRD: std::sync::OnceLock<Desc> = std::sync::OnceLock::new();
        let desc = WEIRD.get_or_init(|| harness_desc(weird_status_hook));
        let desc: &'static Desc = unsafe { &*(desc as *const Desc) };
        let dir = test_dir("weird-status");
        let set = set_with(desc, &dir);
        let entry = dir.join("main.lmn");
        let err = set
            .transform_source(SourceKind::Css, "x".to_string(), &entry, &entry)
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown status 99"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn emit_write_failures_name_the_path() {
        use std::os::unix::fs::PermissionsExt;
        let dir = test_dir("write-fails");
        let set = PluginSet {
            plugins: Vec::new(),
            app_dir: dir.clone(),
            check_only: false,
        };
        let out = |path: &str| Output {
            path: path.to_string(),
            bytes: b"x".to_vec(),
        };

        // The per-plugin root exists as a plain file: the reset fails.
        std::fs::create_dir_all(dir.join(".lumen/generated")).unwrap();
        std::fs::write(dir.join(".lumen/generated/demo"), b"file").unwrap();
        let err = set.write_outputs("demo", &[out("a.txt")]).unwrap_err();
        assert!(matches!(err, PluginError::Write { .. }), "{err}");
        std::fs::remove_file(dir.join(".lumen/generated/demo")).unwrap();

        // The generated root is read-only: creating the plugin dir fails.
        let generated = dir.join(".lumen/generated");
        std::fs::set_permissions(&generated, std::fs::Permissions::from_mode(0o555)).unwrap();
        let err = set.write_outputs("demo", &[out("a.txt")]).unwrap_err();
        std::fs::set_permissions(&generated, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(err, PluginError::Write { .. }), "{err}");

        // The destination collides with a directory an earlier output made.
        let err = set
            .write_outputs("demo", &[out("sub/a.txt"), out("sub")])
            .unwrap_err();
        assert!(matches!(err, PluginError::Write { .. }), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
