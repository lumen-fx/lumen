//! Archives the tests build for themselves.
//!
//! An archive is a binary, and a hostile archive is a binary nobody wants
//! checked out on their machine, so this crate ships none. Every fixture the
//! suite reads is written here, at the moment it is needed, from the same
//! crates the module unpacks with. Each function takes the full path to
//! write, so a test can save one under a name that disagrees with its
//! contents and check that the bytes still decide.
//!
//! Not part of the module's surface: this exists for the tests and carries no
//! stability promise.

use std::io::Write;
use std::path::Path;

use zip::write::SimpleFileOptions;

/// The files every well-formed fixture carries, and what is in them.
pub const MEMBERS: [(&str, &str); 3] = [
    ("top.txt", "top"),
    ("nested/other.txt", "other"),
    ("nested/deep/leaf.txt", "leaf"),
];

/// The name of the entry that climbs out of the destination.
pub const ESCAPING_ENTRY: &str = "../escape.txt";

/// The name of the entry that spells an absolute destination.
pub const ABSOLUTE_ENTRY: &str = "/tmp/lumen-archive-escape.txt";

/// The name of the symbolic link a fixture carries.
#[cfg(unix)]
pub const LINK_ENTRY: &str = "link.txt";

/// A well-formed zip: the three members, with a directory entry for each
/// level the way an archiver writes one.
pub fn normal_zip(path: &Path) -> std::io::Result<()> {
    let mut writer = zip::ZipWriter::new(std::fs::File::create(path)?);
    let options = SimpleFileOptions::default();
    for directory in ["nested/", "nested/deep/"] {
        writer.add_directory(directory, options).map_err(other)?;
    }
    for (name, body) in MEMBERS {
        writer.start_file(name, options).map_err(other)?;
        writer.write_all(body.as_bytes())?;
    }
    writer.finish().map_err(other)?;
    Ok(())
}

/// A well-formed gzip-compressed tar carrying the same three members.
pub fn normal_tar_gz(path: &Path) -> std::io::Result<()> {
    let encoder =
        flate2::write::GzEncoder::new(std::fs::File::create(path)?, flate2::Compression::fast());
    let mut builder = tar::Builder::new(encoder);
    for (name, body) in MEMBERS {
        append(&mut builder, name, body.as_bytes())?;
    }
    builder.into_inner()?.finish()?;
    Ok(())
}

/// A well-formed uncompressed tar carrying the same three members.
pub fn normal_tar(path: &Path) -> std::io::Result<()> {
    let mut builder = tar::Builder::new(std::fs::File::create(path)?);
    for (name, body) in MEMBERS {
        append(&mut builder, name, body.as_bytes())?;
    }
    builder.into_inner()?;
    Ok(())
}

/// A zip whose second entry climbs out of the destination with `..`. The
/// first entry is ordinary, so a run that stops at the second has already
/// written something.
pub fn escaping_zip(path: &Path) -> std::io::Result<()> {
    let mut writer = zip::ZipWriter::new(std::fs::File::create(path)?);
    let options = SimpleFileOptions::default();
    writer.start_file("top.txt", options).map_err(other)?;
    writer.write_all(b"top")?;
    writer.start_file(ESCAPING_ENTRY, options).map_err(other)?;
    writer.write_all(b"owned")?;
    writer.finish().map_err(other)?;
    Ok(())
}

/// A gzip-compressed tar whose entry names an absolute path.
pub fn absolute_tar_gz(path: &Path) -> std::io::Result<()> {
    let encoder =
        flate2::write::GzEncoder::new(std::fs::File::create(path)?, flate2::Compression::fast());
    let mut builder = tar::Builder::new(encoder);
    let body = b"owned";
    let mut header = tar::Header::new_gnu();
    header.set_size(body.len() as u64);
    header.set_mode(0o644);
    header.set_path_absolute(ABSOLUTE_ENTRY)?;
    header.set_cksum();
    builder.append(&header, &body[..])?;
    builder.into_inner()?.finish()?;
    Ok(())
}

/// A zip carrying one real file and one symbolic link pointing at it.
///
/// Unix only. The link is an archive entry rather than a link on disk, but
/// what a link means is a Unix idea, and the skip it exercises is asserted
/// where the platform has one.
#[cfg(unix)]
pub fn symlink_zip(path: &Path) -> std::io::Result<()> {
    let mut writer = zip::ZipWriter::new(std::fs::File::create(path)?);
    let options = SimpleFileOptions::default();
    writer.start_file("real.txt", options).map_err(other)?;
    writer.write_all(b"real")?;
    writer
        .add_symlink(LINK_ENTRY, "real.txt", options)
        .map_err(other)?;
    writer.finish().map_err(other)?;
    Ok(())
}

/// A file that opens with the zip magic and stops in the middle.
pub fn truncated_zip(path: &Path) -> std::io::Result<()> {
    let mut whole = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut whole);
        writer
            .start_file("top.txt", SimpleFileOptions::default())
            .map_err(other)?;
        writer.write_all(b"top")?;
        writer.finish().map_err(other)?;
    }
    let bytes = whole.into_inner();
    std::fs::write(path, &bytes[..bytes.len() / 2])
}

/// One tar member, written with the mode and size a builder needs set first.
fn append<W: Write>(builder: &mut tar::Builder<W>, name: &str, body: &[u8]) -> std::io::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(body.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append_data(&mut header, name, body)
}

/// A zip error as the io error the fixture writers report.
fn other(error: zip::result::ZipError) -> std::io::Error {
    std::io::Error::other(error)
}
