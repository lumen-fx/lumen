//! `.lpak` resource bundle: a zip-like archive that ships an entire
//! Lumen app's assets (main.lmn, main.css, main.rhai, images, fonts)
//! as a single file. Mirrors Qt's `qrc` and GTK's `GResource`.
//!
//! The on-disk format is intentionally tiny - no compression, no
//! checksums, just a header table and a packed payload region. The
//! goal is a `lumenc bundle <dir> <out.lpak>` round trip that's
//! comprehensible at a hex dump:
//!
//! ```text
//!     +-----------+----------------+-------------------+
//!     | magic     | "LMNB"         | 4 bytes           |
//!     | version   | u32 LE         | 4 bytes           |
//!     | file_count| u32 LE         | 4 bytes           |
//!     +-----------+----------------+-------------------+
//!     | entry[0]  | name_len: u32  | 4 bytes           |
//!     |           | name: utf-8    | name_len bytes    |
//!     |           | offset: u64    | 8 bytes           |
//!     |           | size: u64      | 8 bytes           |
//!     +-----------+----------------+-------------------+
//!     | ...         | ...              | ...                 |
//!     +-----------+----------------+-------------------+
//!     | file_bytes[0]              | size bytes        |
//!     | file_bytes[1]              | size bytes        |
//!     | ...                          | ...                 |
//!     +-------------------------------------------------+
//! ```
//!
//! All multi-byte integers are little-endian. Offsets are absolute
//! byte positions into the bundle blob.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;

/// 4-byte magic prefix identifying a `.lpak` archive. ASCII "LMNB".
pub const MAGIC: [u8; 4] = *b"LMNB";

/// Format version. Bumped on incompatible header changes.
pub const VERSION: u32 = 1;

/// Errors produced when opening / reading a `.lpak`.
#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    /// Underlying I/O failure.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Magic prefix mismatch.
    #[error("bad magic (expected LMNB)")]
    BadMagic,
    /// Header version we don't know how to read.
    #[error("unsupported bundle version {0}")]
    UnsupportedVersion(u32),
    /// Entry name failed UTF-8 decode.
    #[error("entry name not utf-8")]
    BadUtf8,
    /// Entry's offset+size lands outside the bundle blob.
    #[error("entry offset/size out of range")]
    OutOfRange,
}

/// One file inside a [`LumenBundle`].
#[derive(Debug, Clone)]
struct Entry {
    offset: u64,
    size: u64,
}

/// In-memory `.lpak` archive. Holds the raw blob plus an index from
/// virtual path -> byte range so reads are O(1).
#[derive(Clone)]
pub struct LumenBundle {
    /// Raw bundle bytes. Shared via `Arc` so `read` can hand back a
    /// fresh `Vec<u8>` without re-reading from disk.
    blob: Arc<[u8]>,
    /// Logical path -> `(offset, size)` lookup.
    index: HashMap<String, Entry>,
}

impl LumenBundle {
    /// Open a `.lpak` from disk, parse the header, and build the
    /// in-memory index. The full blob is read into memory; typical
    /// bundles are < 50 MiB so this is fine for the v1 use cases.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, BundleError> {
        let mut file = File::open(path.as_ref())?;
        let mut blob = Vec::new();
        file.read_to_end(&mut blob)?;
        Self::from_bytes(blob)
    }

    /// Build a bundle from raw bytes (e.g. `include_bytes!("app.lpak")`).
    pub fn from_bytes(blob: impl Into<Vec<u8>>) -> Result<Self, BundleError> {
        let blob = blob.into();
        let mut cursor = std::io::Cursor::new(&blob);
        let mut magic = [0u8; 4];
        cursor.read_exact(&mut magic)?;
        if magic != MAGIC {
            return Err(BundleError::BadMagic);
        }
        let version = read_u32(&mut cursor)?;
        if version != VERSION {
            return Err(BundleError::UnsupportedVersion(version));
        }
        let file_count = read_u32(&mut cursor)?;
        let mut index = HashMap::with_capacity(file_count as usize);
        for _ in 0..file_count {
            let name_len = read_u32(&mut cursor)? as usize;
            let mut name_buf = vec![0u8; name_len];
            cursor.read_exact(&mut name_buf)?;
            let name = String::from_utf8(name_buf).map_err(|_| BundleError::BadUtf8)?;
            let offset = read_u64(&mut cursor)?;
            let size = read_u64(&mut cursor)?;
            if offset.saturating_add(size) > blob.len() as u64 {
                return Err(BundleError::OutOfRange);
            }
            index.insert(name, Entry { offset, size });
        }
        Ok(Self {
            blob: Arc::from(blob),
            index,
        })
    }

    /// Number of files in the archive.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Is the archive empty?
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// List logical entry names in unspecified order. Useful for
    /// font-discovery passes that filter on extension.
    pub fn entries(&self) -> impl Iterator<Item = &str> {
        self.index.keys().map(String::as_str)
    }

    /// Look up the raw bytes for `name`. Returns `None` when the
    /// entry isn't in the archive. The returned `Vec<u8>` is a fresh
    /// owned copy - bundles are immutable so the slice could in
    /// theory be loaned, but mirroring `Vec<u8>` keeps the call site
    /// uniform with disk-loaded paths.
    pub fn read(&self, name: &str) -> Option<Vec<u8>> {
        let entry = self.index.get(name)?;
        let start = entry.offset as usize;
        let end = start + entry.size as usize;
        self.blob.get(start..end).map(|s| s.to_vec())
    }

    /// Whether `name` exists in the archive.
    pub fn contains(&self, name: &str) -> bool {
        self.index.contains_key(name)
    }

    /// Borrow the entry bytes without copying. Used by the font-db
    /// registration path which feeds the slice straight into
    /// `cosmic_text::fontdb::Database::load_font_data` (which copies
    /// internally anyway).
    pub fn slice(&self, name: &str) -> Option<&[u8]> {
        let entry = self.index.get(name)?;
        let start = entry.offset as usize;
        let end = start + entry.size as usize;
        self.blob.get(start..end)
    }

    /// Package a directory tree into a `.lpak` written to `out`.
    /// `root` is walked recursively; each visited file becomes an
    /// entry keyed by its path RELATIVE to `root`, with forward
    /// slashes. Symlinks are followed.
    ///
    /// A `src/` directory at `root` is left out: an app's code is compiled,
    /// never looked up by name at run time, so a bundle holds assets only.
    ///
    /// Returns the number of entries written.
    pub fn pack_dir(root: impl AsRef<Path>, out: impl AsRef<Path>) -> Result<usize, BundleError> {
        let root = root.as_ref();
        let mut entries: Vec<(String, std::path::PathBuf)> = Vec::new();
        walk_dir(root, root, &mut entries)?;
        // Stable order so two runs produce the same bytes.
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        let mut file = File::create(out.as_ref())?;
        write_bundle(&mut file, &entries)?;
        Ok(entries.len())
    }
}

impl std::fmt::Debug for LumenBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LumenBundle")
            .field("entries", &self.index.len())
            .field("bytes", &self.blob.len())
            .finish()
    }
}

fn read_u32(cur: &mut std::io::Cursor<&Vec<u8>>) -> Result<u32, BundleError> {
    let mut buf = [0u8; 4];
    cur.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn read_u64(cur: &mut std::io::Cursor<&Vec<u8>>) -> Result<u64, BundleError> {
    let mut buf = [0u8; 8];
    cur.read_exact(&mut buf)?;
    Ok(u64::from_le_bytes(buf))
}

/// Recursively collect `(virtual_path, real_path)` pairs under `root`.
/// Skips dotfiles + the `target/` directory at any depth.
fn walk_dir(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, std::path::PathBuf)>,
) -> Result<(), BundleError> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        // Skip dotfiles, `target/`, and known editor noise. The app's `src/`
        // goes too, but only at the root: deeper down the name is an asset
        // folder's, not the app's code.
        if name_str.starts_with('.') || name_str == "target" || (dir == root && name_str == "src") {
            continue;
        }
        let metadata = std::fs::metadata(&path)?;
        if metadata.is_dir() {
            walk_dir(root, &path, out)?;
        } else if metadata.is_file() {
            let rel = path
                .strip_prefix(root)
                .map_err(|_| std::io::Error::other("strip_prefix"))?;
            // Forward-slash logical paths - Lumen URIs are URL-shaped.
            let logical: String = rel
                .components()
                .filter_map(|c| match c {
                    std::path::Component::Normal(s) => Some(s.to_string_lossy().to_string()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("/");
            out.push((logical, path));
        }
    }
    Ok(())
}

/// Encode `entries` into the bundle wire format and stream into `w`.
fn write_bundle<W: Write + Seek>(
    w: &mut W,
    entries: &[(String, std::path::PathBuf)],
) -> Result<(), BundleError> {
    // Header.
    w.write_all(&MAGIC)?;
    w.write_all(&VERSION.to_le_bytes())?;
    w.write_all(&(entries.len() as u32).to_le_bytes())?;
    // First pass: write the entry table with placeholder offsets so
    // we can come back and overwrite once we know where each file
    // payload landed. Capture the byte position of each `offset`
    // field so the second pass can seek + rewrite.
    let mut offset_positions: Vec<u64> = Vec::with_capacity(entries.len());
    for (name, _) in entries {
        let bytes = name.as_bytes();
        w.write_all(&(bytes.len() as u32).to_le_bytes())?;
        w.write_all(bytes)?;
        let pos = w.stream_position()?;
        offset_positions.push(pos);
        // Placeholder offset + size.
        w.write_all(&0u64.to_le_bytes())?;
        w.write_all(&0u64.to_le_bytes())?;
    }
    // Second pass: append each file's payload, capture (offset, size),
    // then seek back and patch the header entries.
    let mut payloads: Vec<(u64, u64)> = Vec::with_capacity(entries.len());
    for (_, real) in entries {
        let mut data = Vec::new();
        File::open(real)?.read_to_end(&mut data)?;
        let offset = w.stream_position()?;
        w.write_all(&data)?;
        payloads.push((offset, data.len() as u64));
    }
    // Patch entries.
    for (pos, (offset, size)) in offset_positions.iter().zip(payloads.iter()) {
        w.seek(SeekFrom::Start(*pos))?;
        w.write_all(&offset.to_le_bytes())?;
        w.write_all(&size.to_le_bytes())?;
    }
    w.seek(SeekFrom::End(0))?;
    Ok(())
}

/// Parse a `lumen://app/<path>` URI and return the `<path>` portion
/// (suitable for `LumenBundle::read`). Returns `None` for any other
/// scheme.
pub fn parse_lumen_uri(uri: &str) -> Option<&str> {
    let rest = uri.strip_prefix("lumen://app/")?;
    Some(rest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn round_trip(entries: &[(&str, &[u8])]) -> LumenBundle {
        let tmp = tempfile_path("bundle");
        // Hand-build the bundle in memory using a Cursor so we don't
        // depend on a tempdir crate.
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut cur = Cursor::new(&mut buf);
            // Header.
            cur.write_all(&MAGIC).unwrap();
            cur.write_all(&VERSION.to_le_bytes()).unwrap();
            cur.write_all(&(entries.len() as u32).to_le_bytes())
                .unwrap();
            // Reserve table.
            let mut offset_positions: Vec<u64> = Vec::new();
            for (name, _) in entries {
                let bytes = name.as_bytes();
                cur.write_all(&(bytes.len() as u32).to_le_bytes()).unwrap();
                cur.write_all(bytes).unwrap();
                offset_positions.push(cur.position());
                cur.write_all(&0u64.to_le_bytes()).unwrap();
                cur.write_all(&0u64.to_le_bytes()).unwrap();
            }
            // Payloads.
            let mut payloads = Vec::new();
            for (_, data) in entries {
                let pos = cur.position();
                cur.write_all(data).unwrap();
                payloads.push((pos, data.len() as u64));
            }
            // Patch.
            for (pos, (off, sz)) in offset_positions.iter().zip(payloads.iter()) {
                cur.set_position(*pos);
                cur.write_all(&off.to_le_bytes()).unwrap();
                cur.write_all(&sz.to_le_bytes()).unwrap();
            }
        }
        let _ = std::fs::write(&tmp, &buf);
        let bundle = LumenBundle::from_bytes(buf).expect("parse bundle");
        let _ = std::fs::remove_file(tmp);
        bundle
    }

    fn tempfile_path(tag: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        // Cheap unique-ish suffix; we're in a unit test, collisions are
        // OK as long as we clean up.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("lumen-bundle-{tag}-{nanos}.lpak"));
        p
    }

    #[test]
    fn read_round_trip() {
        let bundle = round_trip(&[("main.lmn", b"hello"), ("icons/x.png", &[1, 2, 3])]);
        assert_eq!(bundle.read("main.lmn"), Some(b"hello".to_vec()));
        assert_eq!(bundle.read("icons/x.png"), Some(vec![1u8, 2, 3]));
        assert_eq!(bundle.read("missing"), None);
        assert!(bundle.contains("main.lmn"));
        assert_eq!(bundle.len(), 2);
    }

    #[test]
    fn pack_dir_then_open() {
        let dir = tempfile_path("pack-src");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("main.lmn"), b"root").unwrap();
        std::fs::write(dir.join("sub").join("nested.css"), b"body{}").unwrap();
        let out = tempfile_path("pack-out");
        let count = LumenBundle::pack_dir(&dir, &out).expect("pack");
        assert_eq!(count, 2);
        let bundle = LumenBundle::open(&out).expect("open");
        assert_eq!(bundle.read("main.lmn"), Some(b"root".to_vec()));
        assert_eq!(bundle.read("sub/nested.css"), Some(b"body{}".to_vec()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn parse_uri_yields_path() {
        assert_eq!(
            parse_lumen_uri("lumen://app/icons/sun.png"),
            Some("icons/sun.png")
        );
        assert_eq!(parse_lumen_uri("lumen://app/main.lmn"), Some("main.lmn"));
        assert_eq!(parse_lumen_uri("file:///abs/path"), None);
        assert_eq!(parse_lumen_uri("icons/sun.png"), None);
    }

    #[test]
    fn bad_magic_rejected() {
        let mut blob = vec![0u8; 32];
        blob[0..4].copy_from_slice(b"XXXX");
        assert!(matches!(
            LumenBundle::from_bytes(blob),
            Err(BundleError::BadMagic)
        ));
    }
}
