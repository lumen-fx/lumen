//! The Lumen app launcher: the executable a packaged app is built from.
//!
//! `lumenc package` copies this binary, appends the app's compiled artifact to
//! the copy, and puts liblumen and the app's files beside it. Started by an end
//! user, the result finds its own artifact, opens the shared library sitting
//! next to it, and runs the app. Nothing is compiled and no toolchain is
//! involved; see `docs/docs/guides/packaging.md`.
//!
//! The artifact reaches the launcher one of two ways:
//!
//! - appended to the executable itself, with a trailing footer marking where it
//!   starts. Windows and Linux program loaders ignore bytes past the end of the
//!   image, so this is a plain file append.
//! - as a `<name>.lmna` file beside the executable. This is what a macOS
//!   package cross-built from another platform ships, where appending is not
//!   available (a Mach-O signature has to cover the whole file).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Marks an appended artifact. The last bytes of a packaged executable are
/// this magic followed by the payload length, and the payload sits directly
/// before them.
const FOOTER_MAGIC: &[u8; 8] = b"LMNAPACK";

/// Magic plus a little-endian `u64` length.
const FOOTER_LEN: usize = FOOTER_MAGIC.len() + 8;

fn main() -> ExitCode {
    let mut headless = false;
    let mut ticks: u32 = 1;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--headless" => headless = true,
            "--ticks" => match args.next().and_then(|v| v.parse::<u32>().ok()) {
                Some(n) => ticks = n,
                None => {
                    eprintln!("--ticks needs a whole number of ticks");
                    return ExitCode::from(2);
                }
            },
            // Anything else belongs to the app, not to the launcher.
            _ => {}
        }
    }

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("cannot locate this executable: {e}");
            return ExitCode::FAILURE;
        }
    };
    let base_dir = exe.parent().unwrap_or(Path::new(".")).to_path_buf();

    let bytes = match load_artifact(&exe) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };

    match lumenc::loader::run_via_dlopen(&bytes, &base_dir, headless.then_some(ticks)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// The app's artifact bytes: appended to `exe` if it carries a footer,
/// otherwise read from the sidecar file beside it.
///
/// Only the footer and the payload are read, never the program image around
/// them, so an app starts without pulling its own executable through memory.
fn load_artifact(exe: &Path) -> Result<Vec<u8>, String> {
    use std::io::{Read, Seek, SeekFrom};

    let read_error = |e: std::io::Error| format!("read {}: {e}", exe.display());
    let mut file = std::fs::File::open(exe).map_err(read_error)?;
    let total = file.metadata().map_err(read_error)?.len();
    if total >= FOOTER_LEN as u64 {
        file.seek(SeekFrom::End(-(FOOTER_LEN as i64)))
            .map_err(read_error)?;
        let mut footer = [0u8; FOOTER_LEN];
        file.read_exact(&mut footer).map_err(read_error)?;
        if let Some(range) = payload_range(total, &footer) {
            let mut bytes = vec![0u8; (range.end - range.start) as usize];
            file.seek(SeekFrom::Start(range.start))
                .map_err(read_error)?;
            file.read_exact(&mut bytes).map_err(read_error)?;
            return Ok(bytes);
        }
    }

    let sidecar = sidecar_path(exe);
    std::fs::read(&sidecar).map_err(|e| {
        format!(
            "this executable carries no app, and no app was found at {}: {e}",
            sidecar.display()
        )
    })
}

/// Where the sidecar artifact lives: beside the executable, named after it.
fn sidecar_path(exe: &Path) -> PathBuf {
    let stem = exe
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "app".to_string());
    exe.with_file_name(format!("{stem}.lmna"))
}

/// Where the appended artifact sits in a file of `total` bytes ending in
/// `footer`, or `None` when the footer is a foreign one or declares a length
/// that does not fit in the bytes before it.
fn payload_range(total: u64, footer: &[u8; FOOTER_LEN]) -> Option<std::ops::Range<u64>> {
    if &footer[..FOOTER_MAGIC.len()] != FOOTER_MAGIC {
        return None;
    }
    let mut len = [0u8; 8];
    len.copy_from_slice(&footer[FOOTER_MAGIC.len()..]);
    let len = u64::from_le_bytes(len);
    if len == 0 {
        return None;
    }
    let end = total.checked_sub(FOOTER_LEN as u64)?;
    let start = end.checked_sub(len)?;
    Some(start..end)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The byte range of the appended artifact in a whole image, or `None`
    /// when it carries no usable footer. The launcher itself seeks to the
    /// footer rather than holding the image, so this is how the tests reach
    /// the same decision over a slice.
    fn appended_artifact(image: &[u8]) -> Option<std::ops::Range<usize>> {
        if image.len() < FOOTER_LEN {
            return None;
        }
        let mut footer = [0u8; FOOTER_LEN];
        footer.copy_from_slice(&image[image.len() - FOOTER_LEN..]);
        let range = payload_range(image.len() as u64, &footer)?;
        Some(range.start as usize..range.end as usize)
    }

    fn with_footer(stub: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut out = stub.to_vec();
        out.extend_from_slice(payload);
        out.extend_from_slice(FOOTER_MAGIC);
        out.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        out
    }

    #[test]
    fn reads_an_appended_payload() {
        let image = with_footer(b"stub-image-bytes", b"LMNA-payload");
        let range = appended_artifact(&image).expect("footer found");
        assert_eq!(&image[range], b"LMNA-payload");
    }

    #[test]
    fn a_plain_executable_has_no_payload() {
        assert!(appended_artifact(b"an ordinary executable image").is_none());
        assert!(appended_artifact(b"").is_none());
    }

    /// A short read, a truncated payload, and an overlong declared length all
    /// have to come back empty rather than panic on the slice.
    #[test]
    fn a_truncated_image_has_no_payload() {
        let image = with_footer(b"stub", b"payload");
        assert!(appended_artifact(&image[..FOOTER_LEN - 1]).is_none());

        let mut overlong = image.clone();
        let len = overlong.len();
        overlong[len - 8..].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(appended_artifact(&overlong).is_none());

        let mut empty_payload = Vec::new();
        empty_payload.extend_from_slice(FOOTER_MAGIC);
        empty_payload.extend_from_slice(&0u64.to_le_bytes());
        assert!(appended_artifact(&empty_payload).is_none());
    }

    #[test]
    fn the_sidecar_is_named_after_the_executable() {
        assert_eq!(
            sidecar_path(Path::new("/apps/Notes/Notes")),
            PathBuf::from("/apps/Notes/Notes.lmna")
        );
        assert_eq!(
            sidecar_path(Path::new("/apps/Notes/Notes.exe")),
            PathBuf::from("/apps/Notes/Notes.lmna")
        );
    }
}
