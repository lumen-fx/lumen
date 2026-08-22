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
    #[error("plugin '{name}': failed to open {path}: {source}")]
    Open {
        name: String,
        path: PathBuf,
        source: libloading::Error,
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
    /// A set with no plugins; every hook is a no-op.
    pub fn empty() -> Self {
        PluginSet {
            plugins: Vec::new(),
            app_dir: PathBuf::new(),
            check_only: false,
        }
    }

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
            let lib = unsafe { Library::new(&path) }.map_err(|source| PluginError::Open {
                name: cfg.name.clone(),
                path: path.clone(),
                source,
            })?;
            let entry: libloading::Symbol<unsafe extern "C" fn() -> *const Desc> =
                unsafe { lib.get(abi::ENTRY) }.map_err(|_| PluginError::MissingEntry {
                    name: cfg.name.clone(),
                    path: path.clone(),
                })?;
            let desc = unsafe { entry() };
            if desc.is_null() {
                return Err(PluginError::NullDescriptor {
                    name: cfg.name.clone(),
                });
            }
            verify(&cfg.name, unsafe { &*desc })?;
            let config_toml = toml::to_string(&cfg.config).unwrap_or_default();
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
            app_dir: app_dir.to_path_buf(),
            check_only: false,
        })
    }

    /// Mark the set as running under `lumenc check`: hooks still run, emit
    /// outputs are discarded.
    pub fn check_only(mut self, yes: bool) -> Self {
        self.check_only = yes;
        self
    }

    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
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
                bytes = Some(codec::encode(&*ir).map_err(|e| PluginError::Codec {
                    plugin: plugin.name.clone(),
                    hook: "transform_ir",
                    message: e,
                })?);
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
        for plugin in &self.plugins {
            let desc = plugin.desc();
            if desc.lint.is_none() && desc.emit.is_none() {
                continue;
            }
            if bytes.is_none() {
                bytes = Some(codec::encode(ir).map_err(|e| PluginError::Codec {
                    plugin: plugin.name.clone(),
                    hook: "lint",
                    message: e,
                })?);
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
                    if !self.check_only {
                        self.write_outputs(&plugin.name, &outputs)?;
                    }
                }
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
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).map_err(|source| PluginError::Write {
                    plugin: plugin.to_string(),
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            std::fs::write(&dest, &out.bytes).map_err(|source| PluginError::Write {
                plugin: plugin.to_string(),
                path: dest.clone(),
                source,
            })?;
        }
        Ok(())
    }

    /// Render plugin findings in the same shape as the built-in lint lines:
    /// severity, anchor, `[<plugin>/<rule>]`, message, optional hint.
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
/// byte payloads or the pointers untrustworthy. Field order matters: the
/// first two fields sit at frozen offsets and are read before the rest of
/// the struct is believed.
fn verify(declared: &str, desc: &Desc) -> Result<(), PluginError> {
    let name = declared.to_string();
    if desc.abi_version != abi::ABI_VERSION {
        return Err(PluginError::AbiMismatch {
            name,
            want: abi::ABI_VERSION,
            got: desc.abi_version,
        });
    }
    if (desc.struct_size as usize) < std::mem::size_of::<Desc>() {
        return Err(PluginError::BadDescriptor {
            name,
            reason: format!(
                "descriptor is {} bytes, expected at least {}",
                desc.struct_size,
                std::mem::size_of::<Desc>()
            ),
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
            _reserved: 0,
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
        verify("demo", &good_desc()).unwrap();
    }

    #[test]
    fn wrong_abi_version_is_refused() {
        let mut d = good_desc();
        d.abi_version = ABI_VERSION + 1;
        let err = verify("demo", &d).unwrap_err().to_string();
        assert!(err.contains("plugin 'demo'"), "{err}");
        assert!(err.contains("ABI"), "{err}");
    }

    #[test]
    fn short_struct_is_refused() {
        let mut d = good_desc();
        d.struct_size = 8;
        let err = verify("demo", &d).unwrap_err().to_string();
        assert!(err.contains("descriptor is 8 bytes"), "{err}");
    }

    #[test]
    fn wrong_ir_version_is_refused() {
        let mut d = good_desc();
        d.ir_format_version = 1;
        let err = verify("demo", &d).unwrap_err().to_string();
        assert!(err.contains("IR format"), "{err}");
        assert!(err.contains("matching Lumen tag"), "{err}");
    }

    #[test]
    fn null_name_is_refused() {
        let mut d = good_desc();
        d.name = std::ptr::null();
        let err = verify("demo", &d).unwrap_err().to_string();
        assert!(err.contains("null name"), "{err}");
    }

    #[test]
    fn name_mismatch_names_both() {
        let err = verify("other", &good_desc()).unwrap_err().to_string();
        assert!(err.contains("'other'"), "{err}");
        assert!(err.contains("'demo'"), "{err}");
    }

    #[test]
    fn missing_free_is_refused() {
        let mut d = good_desc();
        d.free = None;
        let err = verify("demo", &d).unwrap_err().to_string();
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
}
