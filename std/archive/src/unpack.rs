//! Reading an archive out onto disk: format detection, the path guard, and
//! the two unpackers behind them.
//!
//! Nothing here touches the app or the ECS, so the rules an app depends on
//! are testable on their own:
//!
//! - **The bytes decide the format.** A file's magic names it; the extension
//!   is consulted only when the magic says nothing, so an archive saved under
//!   the wrong name still unpacks.
//! - **The guard is fail-closed.** An entry that could write outside the
//!   destination stops the whole extraction. Half of a hostile archive on
//!   disk is worse than none of it, and an entry that looks safe only because
//!   the check was lenient is the bug the guard exists to prevent.
//! - **Links are not written.** A symbolic or hard link inside the
//!   destination can point anywhere, and following it afterwards escapes a
//!   check made at write time. They are counted and skipped instead.
//! - **A filter picks files, not the guard.** With a [`Filter`], only the
//!   files it matches are written, but every entry still passes the guard
//!   first: a hostile entry fails the extraction whether or not the filter
//!   would have kept it.

use std::fs::File;
use std::io::{Read, Seek};
use std::path::{Component, Path, PathBuf};

/// The container an archive uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// A `.zip` file.
    Zip,
    /// A gzip-compressed tar (`.tar.gz`, `.tgz`).
    TarGz,
    /// An uncompressed tar (`.tar`).
    Tar,
}

/// How many leading bytes [`detect`] wants. The tar magic sits at offset 257,
/// which is the furthest in of the three.
pub const MAGIC_LEN: u64 = 265;

/// What one extraction wrote.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Unpacked {
    /// Files written. Directories and skipped links are not counted.
    pub files: usize,
    /// Entries passed over because they are links rather than data.
    pub links_skipped: usize,
    /// Files passed over because the filter did not match them.
    pub filtered: usize,
}

/// Which files an extraction keeps: the entries matching any of its glob
/// patterns.
///
/// `*` matches any run of characters and `?` any one character, both inside
/// one path segment; `**` as a whole segment matches any number of segments.
/// A pattern without a `/` is matched against the entry's file name at any
/// depth, so `*.so` keeps `linux/x64/liblwjgl.so`; a pattern with one is
/// matched against the whole path inside the archive. A filter with no
/// patterns keeps nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    patterns: Vec<String>,
}

impl Filter {
    /// A filter keeping the files that match any of `patterns`.
    #[must_use]
    pub fn new<S: Into<String>>(patterns: impl IntoIterator<Item = S>) -> Self {
        Self {
            patterns: patterns.into_iter().map(Into::into).collect(),
        }
    }

    /// Whether the file at `relative`, a path inside the archive, is kept.
    #[must_use]
    pub fn matches(&self, relative: &Path) -> bool {
        let segments: Vec<String> = relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let Some(file_name) = segments.last() else {
            return false;
        };
        self.patterns.iter().any(|pattern| {
            let pattern = pattern.trim_start_matches('/');
            if pattern.contains('/') {
                let parts: Vec<&str> = pattern.split('/').filter(|p| !p.is_empty()).collect();
                path_matches(&parts, &segments)
            } else {
                segment_matches(pattern.as_bytes(), file_name.as_bytes())
            }
        })
    }
}

/// Whether the pattern segments `parts` match the path `segments`, with `**`
/// standing for any number of whole segments.
fn path_matches(parts: &[&str], segments: &[String]) -> bool {
    match parts.split_first() {
        None => segments.is_empty(),
        Some((&"**", rest)) => {
            (0..=segments.len()).any(|skip| path_matches(rest, &segments[skip..]))
        }
        Some((part, rest)) => match segments.split_first() {
            Some((segment, tail)) => {
                segment_matches(part.as_bytes(), segment.as_bytes()) && path_matches(rest, tail)
            }
            None => false,
        },
    }
}

/// Whether one pattern segment matches one path segment, with `*` for any run
/// of characters and `?` for exactly one.
fn segment_matches(pattern: &[u8], text: &[u8]) -> bool {
    match pattern.split_first() {
        None => text.is_empty(),
        Some((b'*', rest)) => (0..=text.len()).any(|skip| segment_matches(rest, &text[skip..])),
        Some((b'?', rest)) => {
            // One character, which in UTF-8 is a lead byte and its
            // continuation bytes.
            let Some((_, tail)) = text.split_first() else {
                return false;
            };
            let continuation = tail.iter().take_while(|b| (**b & 0xC0) == 0x80).count();
            segment_matches(rest, &tail[continuation..])
        }
        Some((head, rest)) => text.first() == Some(head) && segment_matches(rest, &text[1..]),
    }
}

/// Which container `head` holds, falling back to what `name` is called.
///
/// The magic bytes are asked first, so a `.tar.gz` saved as `download.zip`
/// still unpacks as what it is. The extension answers only when the leading
/// bytes match nothing, which is where an empty or unusual archive lands.
#[must_use]
pub fn detect(head: &[u8], name: &Path) -> Option<Format> {
    if head.starts_with(b"PK\x03\x04") {
        return Some(Format::Zip);
    }
    if head.starts_with(&[0x1f, 0x8b]) {
        return Some(Format::TarGz);
    }
    if head.len() >= 262 && &head[257..262] == b"ustar" {
        return Some(Format::Tar);
    }
    let lower = name.to_string_lossy().to_ascii_lowercase();
    if lower.ends_with(".zip") {
        Some(Format::Zip)
    } else if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        Some(Format::TarGz)
    } else if lower.ends_with(".tar") {
        Some(Format::Tar)
    } else {
        None
    }
}

/// Unpack `src` into `dest`, creating `dest` and everything under it. With a
/// `filter`, only the files it matches are written, and only the directories
/// those files need are created.
///
/// An existing file in the way is overwritten. A rejected entry ends the run
/// with an error naming it; whatever earlier entries already wrote stays on
/// disk, so a destination that took a failed extraction is not a place to
/// keep using.
pub fn extract(src: &Path, dest: &Path, filter: Option<&Filter>) -> Result<Unpacked, String> {
    let mut file = File::open(src).map_err(|e| format!("cannot open {}: {e}", src.display()))?;
    let mut head = Vec::new();
    (&mut file)
        .take(MAGIC_LEN)
        .read_to_end(&mut head)
        .map_err(|e| format!("cannot read {}: {e}", src.display()))?;
    let format = detect(&head, src)
        .ok_or_else(|| format!("{} is not a zip, tar, or tar.gz archive", src.display()))?;
    file.rewind()
        .map_err(|e| format!("cannot read {}: {e}", src.display()))?;

    std::fs::create_dir_all(dest).map_err(|e| format!("cannot create {}: {e}", dest.display()))?;
    let root = dest
        .canonicalize()
        .map_err(|e| format!("cannot resolve {}: {e}", dest.display()))?;

    let reader = std::io::BufReader::new(file);
    match format {
        Format::Zip => unzip(reader, &root, filter),
        Format::TarGz => untar(flate2::read::GzDecoder::new(reader), &root, filter),
        Format::Tar => untar(reader, &root, filter),
    }
}

/// Write out a zip.
fn unzip<R: Read + Seek>(
    reader: R,
    root: &Path,
    filter: Option<&Filter>,
) -> Result<Unpacked, String> {
    let mut archive =
        zip::ZipArchive::new(reader).map_err(|e| format!("cannot read the archive: {e}"))?;
    let mut out = Unpacked::default();
    for i in 0..archive.len() {
        let mut member = archive
            .by_index(i)
            .map_err(|e| format!("cannot read the archive: {e}"))?;
        let name = member.name().to_string();
        let relative = guarded(&name)?;
        if member.is_symlink() {
            out.links_skipped += 1;
            continue;
        }
        if member.is_dir() {
            if filter.is_none() && !relative.as_os_str().is_empty() {
                directory(root, &relative, &name)?;
            }
            continue;
        }
        if filter.is_some_and(|f| !f.matches(&relative)) {
            out.filtered += 1;
            continue;
        }
        let target = file_slot(root, &relative, &name)?;
        let mut sink =
            File::create(&target).map_err(|e| format!("cannot write {}: {e}", target.display()))?;
        std::io::copy(&mut member, &mut sink)
            .map_err(|e| format!("cannot write {}: {e}", target.display()))?;
        drop(sink);
        set_mode(&target, member.unix_mode());
        out.files += 1;
    }
    Ok(out)
}

/// Write out a tar, whatever supplied its bytes.
fn untar<R: Read>(reader: R, root: &Path, filter: Option<&Filter>) -> Result<Unpacked, String> {
    let mut archive = tar::Archive::new(reader);
    archive.set_overwrite(true);
    archive.set_preserve_permissions(true);
    let mut out = Unpacked::default();
    let entries = archive
        .entries()
        .map_err(|e| format!("cannot read the archive: {e}"))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("cannot read the archive: {e}"))?;
        let name = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
        let relative = guarded(&name)?;
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            if filter.is_none() && !relative.as_os_str().is_empty() {
                directory(root, &relative, &name)?;
            }
            continue;
        }
        if !kind.is_file() {
            // Symbolic links, hard links, devices, and fifos: everything that
            // is a reference rather than data.
            out.links_skipped += 1;
            continue;
        }
        if filter.is_some_and(|f| !f.matches(&relative)) {
            out.filtered += 1;
            continue;
        }
        let target = file_slot(root, &relative, &name)?;
        entry
            .unpack(&target)
            .map_err(|e| format!("cannot write {}: {e}", target.display()))?;
        out.files += 1;
    }
    Ok(out)
}

/// The relative path an entry may be written at, or the error that ends the
/// extraction.
///
/// Refused: an absolute path, any `..` component, a Windows drive or UNC
/// prefix, and a name carrying a backslash (an archive separates with `/`, so
/// a backslash is either a Windows path or a name that would mean two
/// different things on two platforms). A `.` component is dropped, which is
/// how tar writes an archive rooted at its own directory.
fn guarded(name: &str) -> Result<PathBuf, String> {
    inspect(name).map_err(|why| format!("the entry `{name}` was refused: {why}"))
}

/// [`guarded`] without the message wrapper.
fn inspect(name: &str) -> Result<PathBuf, String> {
    if name.is_empty() {
        return Err("it has no name".to_string());
    }
    if name.contains('\\') {
        return Err(
            "its name carries a backslash, which an archive uses only for a Windows path"
                .to_string(),
        );
    }
    if name.starts_with('/') {
        return Err("it is an absolute path".to_string());
    }
    let mut relative = PathBuf::new();
    for (index, component) in Path::new(name).components().enumerate() {
        match component {
            Component::Normal(part) => {
                let text = part.to_string_lossy();
                let bytes = text.as_bytes();
                if index == 0
                    && bytes.len() >= 2
                    && bytes[1] == b':'
                    && bytes[0].is_ascii_alphabetic()
                {
                    return Err("it opens with a Windows drive letter".to_string());
                }
                relative.push(part);
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err("it climbs above the destination with `..`".to_string());
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err("it is an absolute path".to_string());
            }
        }
    }
    Ok(relative)
}

/// Create one directory entry, proving it landed inside the destination.
fn directory(root: &Path, relative: &Path, name: &str) -> Result<(), String> {
    let target = root.join(relative);
    std::fs::create_dir_all(&target)
        .map_err(|e| format!("cannot create {}: {e}", target.display()))?;
    contained(root, &target, name)?;
    Ok(())
}

/// Where one file entry is written, with its parent created and proven to be
/// inside the destination.
///
/// The components alone cannot climb out, but a symbolic link already sitting
/// in the destination can, so the parent is resolved and checked rather than
/// trusted.
fn file_slot(root: &Path, relative: &Path, name: &str) -> Result<PathBuf, String> {
    let Some(file_name) = relative.file_name() else {
        return Err(format!("the entry `{name}` was refused: it names no file"));
    };
    let target = root.join(relative);
    let parent = target.parent().unwrap_or(root);
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    let parent = contained(root, parent, name)?;
    Ok(parent.join(file_name))
}

/// The resolved form of `path`, refused unless it is `root` or sits under it.
fn contained(root: &Path, path: &Path, name: &str) -> Result<PathBuf, String> {
    let resolved = path
        .canonicalize()
        .map_err(|e| format!("cannot resolve {}: {e}", path.display()))?;
    if !resolved.starts_with(root) {
        return Err(format!(
            "the entry `{name}` was refused: it writes outside the destination directory"
        ));
    }
    Ok(resolved)
}

/// Give a written file the permissions the archive recorded for it.
///
/// An archive written on a system with no Unix modes records none, and one
/// recording all-zero permissions would leave a file nothing can read, so
/// both cases keep what the platform gave the file.
#[cfg(unix)]
fn set_mode(path: &Path, mode: Option<u32>) {
    use std::os::unix::fs::PermissionsExt;

    if let Some(mode) = mode
        && mode & 0o777 != 0
    {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o777));
    }
}

/// Permissions in an archive are a Unix concept; elsewhere a written file
/// keeps what the platform gave it.
#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: Option<u32>) {}
