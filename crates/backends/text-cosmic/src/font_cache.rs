//! Persistent system-font metadata cache.
//!
//! `FontSystem::new()` walks every system font directory and parses each
//! face's name / weight / style / stretch metadata on *every* launch -
//! ~10-13 ms of cold-start cost that is identical run to run until the
//! installed font set changes. This module serialises that face metadata
//! (path + index + families + weight + style + stretch + monospaced -
//! **never the font bytes**, which stay lazily read/mmapped by fontdb on
//! first shape) to `$XDG_CACHE_HOME/lumen/fontdb.bin` and, on the next
//! launch, rebuilds an identical [`cosmic_text::fontdb::Database`] via
//! [`fontdb::Database::push_face_info`] instead of re-walking + re-parsing.
//!
//! ## Correctness
//!
//! fontdb resolves a family query purely by the face's family-name
//! *strings* (see `Database::query`: `face.families.iter().any(|f| f.0 ==
//! name)`), never by the per-name `Language` tag, and cosmic-text's font
//! matching never reads `Language` either. So the reconstructed database
//! produces byte-identical shaping to a fresh scan as long as the family
//! name strings, order, weight, style, stretch, monospaced flag and the
//! five generic-family aliases (serif / sans-serif / monospace / cursive /
//! fantasy - captured from the freshly scanned db and re-applied) are
//! preserved. All of those are cached exactly; the golden-image suite is
//! the regression guard.
//!
//! ## Invalidation
//!
//! The cache records every font *directory* it loaded from (the parents of
//! the scanned face paths) with that directory's mtime. On Linux/macOS a
//! directory's mtime bumps when an entry is added, removed or renamed, so
//! installing or removing a font invalidates the cache and forces a fresh
//! scan + rewrite. A corrupt, truncated or version-mismatched cache file
//! is treated as a miss (never a panic).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use cosmic_text::FontSystem;
use cosmic_text::fontdb::{
    Database, FaceInfo, Family, ID, Language, Source, Stretch, Style, Weight,
};

/// Magic + format version. Bump [`VERSION`] on any layout change, and on
/// any change to what the fresh scan records, so old caches are rejected
/// instead of replayed.
const MAGIC: &[u8; 4] = b"LFDB";
const VERSION: u16 = 2;

/// Build a [`FontSystem`] equivalent to [`FontSystem::new`], using the
/// persistent metadata cache when it is present and still valid. On a
/// cache miss (absent / stale / corrupt) this falls back to the full
/// `FontSystem::new()` scan and best-effort writes a fresh cache for next
/// time. Never fails: any cache I/O error degrades silently to the scan.
pub fn load_font_system() -> FontSystem {
    // Escape hatch: `LUMEN_FONT_CACHE=0` (or `off`/`false`) forces the full
    // scan every time and skips writing, for benchmarking the cold path or
    // sidestepping a suspect cache without deleting the file.
    if cache_disabled() {
        return scan();
    }
    if let Some(path) = cache_path()
        && let Some(cf) = read_valid(&path)
    {
        return build_from_cache(cf);
    }
    // Miss: full scan, then persist for next launch.
    let fs = scan();
    if let Some(path) = cache_path() {
        let _ = write_cache(&path, &fs);
    }
    fs
}

/// Full system-font scan, keeping the generic-family aliases the platform
/// itself resolved.
///
/// `FontSystem::new` loads the system fonts and then overwrites the
/// `serif` / `sans-serif` / `monospace` aliases with three fixed family
/// names, discarding the mapping fontdb had just read from fontconfig.
/// On a machine that has none of those three families the alias names no
/// installed face at all, so every lookup for a generic family misses and
/// the text falls through to cosmic-text's last-resort face list. Scanning
/// the database here keeps the platform's own answer and reaches for
/// cosmic-text's names only when that answer is not installed.
fn scan() -> FontSystem {
    let mut db = Database::new();
    db.load_system_fonts();
    ground_generics(&mut db);
    let locale = sys_locale::get_locale().unwrap_or_else(|| String::from("en-US"));
    FontSystem::new_with_locale_and_db(locale, db)
}

/// Repoint any generic-family alias that names a family the database does
/// not hold, using cosmic-text's built-in choice as the second try. An
/// alias with no installed candidate is left alone.
fn ground_generics(db: &mut Database) {
    let serif = grounded(db, &Family::Serif, "DejaVu Serif");
    let sans_serif = grounded(db, &Family::SansSerif, "Open Sans");
    let monospace = grounded(db, &Family::Monospace, "Noto Sans Mono");
    if let Some(name) = serif {
        db.set_serif_family(name);
    }
    if let Some(name) = sans_serif {
        db.set_sans_serif_family(name);
    }
    if let Some(name) = monospace {
        db.set_monospace_family(name);
    }
}

/// `Some(replacement)` when `generic` names no installed family and `alt`
/// does; `None` to leave the alias as the platform set it.
fn grounded(db: &Database, generic: &Family<'_>, alt: &str) -> Option<String> {
    if family_present(db, db.family_name(generic)) {
        return None;
    }
    family_present(db, alt).then(|| alt.to_string())
}

/// Whether any face in the database lists `name` among its families.
fn family_present(db: &Database, name: &str) -> bool {
    db.faces()
        .any(|f| f.families.iter().any(|(fam, _)| fam.as_str() == name))
}

/// Whether `LUMEN_FONT_CACHE` is set to a falsey value (`0` / `off` /
/// `false`), disabling the persistent cache for this process.
fn cache_disabled() -> bool {
    match std::env::var("LUMEN_FONT_CACHE") {
        Ok(v) => {
            let v = v.trim().to_ascii_lowercase();
            v == "0" || v == "off" || v == "false" || v == "no"
        }
        Err(_) => false,
    }
}

/// One cached face's metadata. Mirrors the subset of
/// [`fontdb::FaceInfo`] that survives a round-trip through
/// [`Database::push_face_info`]; the `id` is reassigned on insert and the
/// source bytes are re-read lazily from `path`.
struct FaceRec {
    path: String,
    index: u32,
    families: Vec<String>,
    post_script_name: String,
    style: u8,
    weight: u16,
    stretch: u8,
    monospaced: bool,
}

/// The five CSS generic-family aliases resolved from the freshly scanned
/// database. Re-applied on rebuild so `font-family: cursive` etc. resolve
/// to the exact same concrete family a fresh scan would pick (fontconfig
/// can set these on Linux).
struct Generics {
    serif: String,
    sans_serif: String,
    monospace: String,
    cursive: String,
    fantasy: String,
}

/// Decoded cache file.
struct CacheFile {
    locale: String,
    /// (directory, mtime-nanos-since-epoch) pairs used for invalidation.
    dirs: Vec<(String, u64)>,
    generics: Generics,
    faces: Vec<FaceRec>,
}

/// Per-user cache directory for the fontdb metadata blob, resolved per OS:
/// - Windows: `%LOCALAPPDATA%`
/// - macOS:   `~/Library/Caches`
/// - Linux/other Unix: `$XDG_CACHE_HOME`, else `$HOME/.cache`
///
/// Returns `None` when no suitable dir is resolvable (sandboxed/CI runs
/// without the relevant env); the caller then simply disables the cache.
fn cache_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("Library").join("Caches"))
            .filter(|p| p.is_absolute())
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
    }
}

/// Full path to the fontdb cache file (`<cache_dir>/lumen/fontdb.bin`).
/// `None` when [`cache_dir`] cannot resolve a base (the cache is then
/// simply disabled).
fn cache_path() -> Option<PathBuf> {
    Some(cache_dir()?.join("lumen").join("fontdb.bin"))
}

/// Directory mtime as nanoseconds since the Unix epoch, or `None` if the
/// path is missing / unstat-able.
fn dir_mtime(dir: &Path) -> Option<u64> {
    let md = std::fs::metadata(dir).ok()?;
    let modified = md.modified().ok()?;
    let dur = modified.duration_since(UNIX_EPOCH).ok()?;
    Some(dur.as_nanos() as u64)
}

/// Read + decode the cache and confirm every recorded directory still has
/// its recorded mtime. Returns `None` (=> rescan) on any mismatch or decode
/// failure.
fn read_valid(path: &Path) -> Option<CacheFile> {
    let bytes = std::fs::read(path).ok()?;
    let cf = decode(&bytes)?;
    if cf.faces.is_empty() || cf.dirs.is_empty() {
        return None;
    }
    for (dir, mtime) in &cf.dirs {
        match dir_mtime(Path::new(dir)) {
            Some(cur) if cur == *mtime => {}
            _ => return None,
        }
    }
    Some(cf)
}

/// Reconstruct a [`FontSystem`] from cached metadata. Faces are inserted
/// via [`Database::push_face_info`] (no file parse); the source bytes are
/// read lazily by fontdb on first shape.
fn build_from_cache(cf: CacheFile) -> FontSystem {
    let mut db = Database::new();
    for f in &cf.faces {
        let families = f
            .families
            .iter()
            // Language is behaviourally irrelevant to fontdb's name-string
            // query and to cosmic-text matching (see module docs); the
            // stored order already had English-US first, per fontdb.
            .map(|n| (n.clone(), Language::English_UnitedStates))
            .collect();
        db.push_face_info(FaceInfo {
            id: ID::dummy(),
            source: Source::File(PathBuf::from(&f.path)),
            index: f.index,
            families,
            post_script_name: f.post_script_name.clone(),
            style: u8_to_style(f.style),
            weight: Weight(f.weight),
            stretch: u8_to_stretch(f.stretch),
            monospaced: f.monospaced,
        });
    }
    // Re-apply the generic aliases exactly as the fresh scan resolved them
    // (see `scan`: the platform's mapping, grounded against the faces it
    // actually holds).
    db.set_serif_family(cf.generics.serif);
    db.set_sans_serif_family(cf.generics.sans_serif);
    db.set_monospace_family(cf.generics.monospace);
    db.set_cursive_family(cf.generics.cursive);
    db.set_fantasy_family(cf.generics.fantasy);
    FontSystem::new_with_locale_and_db(cf.locale, db)
}

/// Serialise a freshly scanned `FontSystem`'s font database to `path`
/// (atomic temp-file + rename). Best-effort: returns `Err` on I/O trouble
/// and the caller ignores it.
fn write_cache(path: &Path, fs: &FontSystem) -> std::io::Result<()> {
    let db = fs.db();
    let mut faces: Vec<FaceRec> = Vec::new();
    // Directory -> mtime, filled as we see each face's parent dir.
    let mut dirs: BTreeMap<String, u64> = BTreeMap::new();
    for info in db.faces() {
        // Only file-backed faces are cacheable (system fonts). Skip
        // in-memory (Binary) faces - none exist for a fresh system scan.
        let path_buf = match &info.source {
            Source::File(p) => p.clone(),
            Source::SharedFile(p, _) => p.clone(),
            // In-memory (Binary) faces are never produced by a system scan.
            Source::Binary(_) => continue,
        };
        if let Some(parent) = path_buf.parent()
            && let Some(dir_str) = parent.to_str()
            && !dirs.contains_key(dir_str)
            && let Some(mt) = dir_mtime(parent)
        {
            dirs.insert(dir_str.to_string(), mt);
        }
        let Some(path_str) = path_buf.to_str() else {
            continue;
        };
        faces.push(FaceRec {
            path: path_str.to_string(),
            index: info.index,
            families: info.families.iter().map(|(n, _)| n.clone()).collect(),
            post_script_name: info.post_script_name.clone(),
            style: style_to_u8(info.style),
            weight: info.weight.0,
            stretch: stretch_to_u8(info.stretch),
            monospaced: info.monospaced,
        });
    }
    let generics = Generics {
        serif: db.family_name(&Family::Serif).to_string(),
        sans_serif: db.family_name(&Family::SansSerif).to_string(),
        monospace: db.family_name(&Family::Monospace).to_string(),
        cursive: db.family_name(&Family::Cursive).to_string(),
        fantasy: db.family_name(&Family::Fantasy).to_string(),
    };
    let cf = CacheFile {
        locale: fs.locale().to_string(),
        dirs: dirs.into_iter().collect(),
        generics,
        faces,
    };
    let bytes = encode(&cf);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Atomic replace so a concurrently starting process never reads a
    // half-written file. Temp name is pid-qualified to avoid collisions.
    let tmp = path.with_extension(format!("bin.tmp.{}", std::process::id()));
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

// --- Style / Stretch <-> u8 ---------------------------------------------

fn style_to_u8(s: Style) -> u8 {
    match s {
        Style::Normal => 0,
        Style::Italic => 1,
        Style::Oblique => 2,
    }
}

fn u8_to_style(v: u8) -> Style {
    match v {
        1 => Style::Italic,
        2 => Style::Oblique,
        _ => Style::Normal,
    }
}

fn stretch_to_u8(s: Stretch) -> u8 {
    match s {
        Stretch::UltraCondensed => 0,
        Stretch::ExtraCondensed => 1,
        Stretch::Condensed => 2,
        Stretch::SemiCondensed => 3,
        Stretch::Normal => 4,
        Stretch::SemiExpanded => 5,
        Stretch::Expanded => 6,
        Stretch::ExtraExpanded => 7,
        Stretch::UltraExpanded => 8,
    }
}

fn u8_to_stretch(v: u8) -> Stretch {
    match v {
        0 => Stretch::UltraCondensed,
        1 => Stretch::ExtraCondensed,
        2 => Stretch::Condensed,
        3 => Stretch::SemiCondensed,
        5 => Stretch::SemiExpanded,
        6 => Stretch::Expanded,
        7 => Stretch::ExtraExpanded,
        8 => Stretch::UltraExpanded,
        _ => Stretch::Normal,
    }
}

// --- Binary codec (little-endian, self-contained, panic-free reader) -----

fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put_str(out: &mut Vec<u8>, s: &str) {
    put_u32(out, s.len() as u32);
    out.extend_from_slice(s.as_bytes());
}

fn encode(cf: &CacheFile) -> Vec<u8> {
    let mut out = Vec::with_capacity(64 * 1024);
    out.extend_from_slice(MAGIC);
    put_u16(&mut out, VERSION);
    put_str(&mut out, &cf.locale);
    for g in [
        &cf.generics.serif,
        &cf.generics.sans_serif,
        &cf.generics.monospace,
        &cf.generics.cursive,
        &cf.generics.fantasy,
    ] {
        put_str(&mut out, g);
    }
    put_u32(&mut out, cf.dirs.len() as u32);
    for (dir, mtime) in &cf.dirs {
        put_str(&mut out, dir);
        put_u64(&mut out, *mtime);
    }
    put_u32(&mut out, cf.faces.len() as u32);
    for f in &cf.faces {
        put_str(&mut out, &f.path);
        put_u32(&mut out, f.index);
        put_str(&mut out, &f.post_script_name);
        out.push(f.style);
        put_u16(&mut out, f.weight);
        out.push(f.stretch);
        out.push(f.monospaced as u8);
        put_u16(&mut out, f.families.len() as u16);
        for name in &f.families {
            put_str(&mut out, name);
        }
    }
    out
}

/// Cursor over the cache bytes; every read is bounds-checked and returns
/// `None` past the end so a truncated/corrupt file can never panic.
struct Cursor<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.p.checked_add(n)?;
        let slice = self.b.get(self.p..end)?;
        self.p = end;
        Some(slice)
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn string(&mut self) -> Option<String> {
        let len = self.u32()? as usize;
        let bytes = self.take(len)?;
        // Font metadata is UTF-8; reject non-UTF-8 rather than lossy-decode
        // so a corrupt entry invalidates the whole cache.
        std::str::from_utf8(bytes).ok().map(str::to_string)
    }
}

fn decode(bytes: &[u8]) -> Option<CacheFile> {
    let mut c = Cursor { b: bytes, p: 0 };
    if c.take(4)? != MAGIC {
        return None;
    }
    if c.u16()? != VERSION {
        return None;
    }
    let locale = c.string()?;
    let generics = Generics {
        serif: c.string()?,
        sans_serif: c.string()?,
        monospace: c.string()?,
        cursive: c.string()?,
        fantasy: c.string()?,
    };
    let n_dirs = c.u32()? as usize;
    // Guard against a corrupt length claiming a huge allocation.
    if n_dirs > 1_000_000 {
        return None;
    }
    let mut dirs = Vec::with_capacity(n_dirs.min(4096));
    for _ in 0..n_dirs {
        let dir = c.string()?;
        let mtime = c.u64()?;
        dirs.push((dir, mtime));
    }
    let n_faces = c.u32()? as usize;
    if n_faces > 10_000_000 {
        return None;
    }
    let mut faces = Vec::with_capacity(n_faces.min(8192));
    for _ in 0..n_faces {
        let path = c.string()?;
        let index = c.u32()?;
        let post_script_name = c.string()?;
        let style = c.u8()?;
        let weight = c.u16()?;
        let stretch = c.u8()?;
        let monospaced = c.u8()? != 0;
        let n_fam = c.u16()? as usize;
        let mut families = Vec::with_capacity(n_fam.min(32));
        for _ in 0..n_fam {
            families.push(c.string()?);
        }
        faces.push(FaceRec {
            path,
            index,
            families,
            post_script_name,
            style,
            weight,
            stretch,
            monospaced,
        });
    }
    Some(CacheFile {
        locale,
        dirs,
        generics,
        faces,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> CacheFile {
        CacheFile {
            locale: "en-US".to_string(),
            dirs: vec![
                ("/usr/share/fonts".to_string(), 123_456_789),
                ("/home/x/.fonts".to_string(), 42),
            ],
            generics: Generics {
                serif: "DejaVu Serif".to_string(),
                sans_serif: "Open Sans".to_string(),
                monospace: "Noto Sans Mono".to_string(),
                cursive: "Comic".to_string(),
                fantasy: "Impact".to_string(),
            },
            faces: vec![
                FaceRec {
                    path: "/usr/share/fonts/DejaVuSans.ttf".to_string(),
                    index: 0,
                    families: vec!["DejaVu Sans".to_string(), "DejaVu Sans Bold".to_string()],
                    post_script_name: "DejaVuSans".to_string(),
                    style: 0,
                    weight: 400,
                    stretch: 4,
                    monospaced: false,
                },
                FaceRec {
                    path: "/usr/share/fonts/mono.ttc".to_string(),
                    index: 2,
                    families: vec!["Mono".to_string()],
                    post_script_name: "Mono-Italic".to_string(),
                    style: 1,
                    weight: 700,
                    stretch: 2,
                    monospaced: true,
                },
            ],
        }
    }

    #[test]
    fn round_trips() {
        let cf = sample();
        let bytes = encode(&cf);
        let back = decode(&bytes).expect("decode ok");
        assert_eq!(back.locale, cf.locale);
        assert_eq!(back.dirs, cf.dirs);
        assert_eq!(back.generics.sans_serif, "Open Sans");
        assert_eq!(back.generics.cursive, "Comic");
        assert_eq!(back.faces.len(), 2);
        assert_eq!(back.faces[1].path, "/usr/share/fonts/mono.ttc");
        assert_eq!(back.faces[1].index, 2);
        assert_eq!(back.faces[1].weight, 700);
        assert_eq!(back.faces[1].stretch, 2);
        assert!(back.faces[1].monospaced);
        assert_eq!(
            back.faces[0].families,
            vec!["DejaVu Sans", "DejaVu Sans Bold"]
        );
    }

    #[test]
    fn rejects_bad_magic() {
        assert!(decode(b"XXXX\x01\x00").is_none());
    }

    #[test]
    fn rejects_truncated() {
        let bytes = encode(&sample());
        assert!(decode(&bytes[..bytes.len() - 3]).is_none());
    }

    #[test]
    fn rejects_version_mismatch() {
        let mut bytes = encode(&sample());
        bytes[4] = 0xFF; // corrupt version low byte
        assert!(decode(&bytes).is_none());
    }

    #[test]
    fn style_stretch_round_trip() {
        for v in 0u8..=2 {
            assert_eq!(style_to_u8(u8_to_style(v)), v);
        }
        for v in 0u8..=8 {
            assert_eq!(stretch_to_u8(u8_to_stretch(v)), v);
        }
    }
}
