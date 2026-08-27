//! The filesystem work behind the `files` script functions, on plain paths.
//!
//! Every operation takes an already-resolved [`Path`] and answers with the
//! value the script sees, or with the one line explaining why it could not.
//! Keeping the refusal as a message rather than printing it here is what lets
//! the plugin apply one warn-and-degrade rule to the whole surface, and what
//! lets these tests read the explanation an author would see.
//!
//! Two rules run through all of it:
//!
//! - **Nothing is recursive.** [`remove`] takes a file or an empty directory,
//!   [`copy`] takes a single file. A script that wants a tree walks it.
//! - **A missing path is an answer, not a fault.** Probing for state that has
//!   not been saved yet is ordinary, so [`read`], [`read_bytes`] and
//!   [`remove`] report nothing when the path is absent.

use std::fs;
use std::io::{ErrorKind, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// What an operation reports when it could not do what was asked: the line an
/// author reads on stderr, without the `lumen-fs: ` prefix.
pub type Refusal = String;

/// The result of one operation: the value, or the refusal to report.
pub type Outcome<T> = Result<T, Refusal>;

/// Whether something exists at `path`. Symlinks are followed, so a link to a
/// file that is gone reads as absent.
pub fn exists(path: &Path) -> bool {
    path.exists()
}

/// Whether `path` is a directory that exists. A file, a missing path, and a
/// path that cannot be read all read as false.
pub fn is_dir(path: &Path) -> bool {
    path.is_dir()
}

/// The names of the entries directly inside `path`, sorted.
///
/// Names, not paths: a script joins them onto the directory it asked about.
/// The listing is one level deep and carries no `.` or `..`.
pub fn list(path: &Path) -> Outcome<Vec<String>> {
    let entries = fs::read_dir(path).map_err(|e| format!("list({}): {e}", path.display()))?;
    let mut names: Vec<String> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("list({}): {e}", path.display()))?;
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    names.sort_unstable();
    Ok(names)
}

/// Create `path` and every missing directory above it. A directory that is
/// already there is success.
pub fn mkdir(path: &Path) -> Outcome<bool> {
    match fs::create_dir_all(path) {
        Ok(()) => Ok(true),
        Err(e) => Err(format!("mkdir({}): {e}", path.display())),
    }
}

/// Remove one file, or one directory that is already empty.
///
/// A directory holding anything is refused: deleting a tree is not something
/// a one-word call should be able to do by accident. A path that is not there
/// answers false without a word, because probing is how a script asks whether
/// it needs to clean up at all.
pub fn remove(path: &Path) -> Outcome<bool> {
    // `symlink_metadata` rather than `metadata`: a symlink pointing at a
    // directory is removed as the link it is, not followed.
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("remove({}): {e}", path.display())),
    };
    let outcome = if meta.is_dir() {
        fs::remove_dir(path).map_err(|e| {
            format!(
                "remove({}): {e}; a directory is removed only when it is empty",
                path.display()
            )
        })
    } else {
        fs::remove_file(path).map_err(|e| format!("remove({}): {e}", path.display()))
    };
    outcome.map(|()| true)
}

/// Copy one file to `dest`, creating the directories `dest` sits under.
///
/// A directory source is refused: this copies a file, and a tree copy is a
/// walk the script writes.
pub fn copy(src: &Path, dest: &Path) -> Outcome<bool> {
    if src.is_dir() {
        return Err(format!(
            "copy({}): a directory is not copied; copy the files inside it",
            src.display()
        ));
    }
    if let Some(parent) = dest.parent()
        && !parent.as_os_str().is_empty()
        && let Err(e) = fs::create_dir_all(parent)
    {
        return Err(format!("copy({}): {e}", parent.display()));
    }
    match fs::copy(src, dest) {
        Ok(_) => Ok(true),
        Err(e) => Err(format!(
            "copy({} -> {}): {e}",
            src.display(),
            dest.display()
        )),
    }
}

/// The utf-8 contents of `path`, or the empty string when it is not there.
pub fn read(path: &Path) -> Outcome<String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("read({}): {e}", path.display())),
    }
}

/// Write `contents` to `path`, replacing what was there.
pub fn write(path: &Path, contents: &str) -> Outcome<bool> {
    match write_atomic(path, contents.as_bytes()) {
        Ok(()) => Ok(true),
        Err(e) => Err(format!("write({}): {e}", path.display())),
    }
}

/// The bytes of `path`, refused when the file is larger than `cap`.
///
/// The cap is what keeps one call from pulling a disk image into a script
/// value; it is the module's `read_bytes_cap` setting. A file exactly the size
/// of the cap is read.
pub fn read_bytes(path: &Path, cap: u64) -> Outcome<Vec<u8>> {
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("read_bytes({}): {e}", path.display())),
    };
    if meta.is_file() && meta.len() > cap {
        return Err(format!(
            "read_bytes({}): the file is {} bytes and the cap is {cap}; raise `read_bytes_cap` \
             in the module's config to read it",
            path.display(),
            meta.len()
        ));
    }
    match fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("read_bytes({}): {e}", path.display())),
    }
}

/// Write `values` to `path` as raw bytes.
///
/// Every element has to be a byte. One that is not refuses the whole write
/// naming its position, rather than truncating or wrapping it into something
/// the script did not ask for.
pub fn write_bytes(path: &Path, values: &[i64]) -> Outcome<bool> {
    let mut bytes = Vec::with_capacity(values.len());
    for (i, value) in values.iter().enumerate() {
        match u8::try_from(*value) {
            Ok(b) => bytes.push(b),
            Err(_) => {
                return Err(format!(
                    "write_bytes({}): element {i} is {value}, and a byte is 0 to 255; nothing \
                     was written",
                    path.display()
                ));
            }
        }
    }
    match write_atomic(path, &bytes) {
        Ok(()) => Ok(true),
        Err(e) => Err(format!("write_bytes({}): {e}", path.display())),
    }
}

/// Write `bytes` to `path` without a truncated-read window.
///
/// `std::fs::write` truncates the file before writing, so a reader racing the
/// write (a file watcher, another process, the app reloading its own save
/// file) can see a zero-length or partial file. Writing to a sibling temp file
/// and renaming it into place avoids that: rename is atomic on a given
/// filesystem, so a concurrent reader always sees either the old contents or
/// the new ones.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let file_name = path
        .file_name()
        .ok_or_else(|| std::io::Error::new(ErrorKind::InvalidInput, "the path names no file"))?;

    // A per-process counter so two writes racing the same path in the same
    // tick never pick the same temp name.
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let tmp_path = dir.join(format!(
        ".{}.tmp-{}-{}",
        file_name.to_string_lossy(),
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));

    let write_result = (|| -> std::io::Result<()> {
        let mut f = fs::File::create(&tmp_path)?;
        f.write_all(bytes)?;
        f.sync_all()
    })();

    match write_result {
        Ok(()) => fs::rename(&tmp_path, path),
        Err(e) => {
            let _ = fs::remove_file(&tmp_path);
            Err(e)
        }
    }
}
