//! Downloads from a Lumen release: one asset at a time, each checked against
//! the release's `sha256sums.txt` before it is used, unpacked beside its
//! final place and moved in, and kept in the user cache directory by version.

use std::fmt::Write as _;
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The marker a finished unpack leaves, so a cache interrupted halfway is
/// fetched again rather than trusted.
pub(crate) const COMPLETE: &str = ".complete";

/// The release's name for a target, the one every asset name carries.
pub(crate) fn target_name(triple: &str) -> Option<&'static str> {
    Some(match triple {
        "x86_64-unknown-linux-gnu" => "linux-x86_64",
        "aarch64-unknown-linux-gnu" => "linux-aarch64",
        "x86_64-apple-darwin" => "macos-x86_64",
        "aarch64-apple-darwin" => "macos-aarch64",
        "x86_64-pc-windows-msvc" => "windows-x86_64",
        "aarch64-pc-windows-msvc" => "windows-aarch64",
        _ => return None,
    })
}

/// The repository releases are fetched from.
pub(crate) fn repo() -> String {
    std::env::var("LUMEN_GH_REPO")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "lumen-fx/lumen".to_string())
}

/// The release's download base for `version`.
pub(crate) fn release_base(version: &str) -> String {
    format!("https://github.com/{}/releases/download/v{version}", repo())
}

/// Where downloads of `what` are kept: the platform cache directory, keyed
/// by version and target so an upgrade never reuses an old copy.
pub(crate) fn cache_dir(what: &str, version: &str, name: &str) -> Option<PathBuf> {
    let base = if cfg!(target_os = "windows") {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library").join("Caches"))
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
    }?;
    Some(base.join("lumen").join(what).join(version).join(name))
}

/// The release's `sha256sums.txt`.
pub(crate) fn checksums(base: &str, offline_hint: &str) -> Result<String, String> {
    let url = format!("{base}/sha256sums.txt");
    let bytes = get(&url).map_err(|e| format!("cannot read {url}: {e}. {offline_hint}"))?;
    String::from_utf8(bytes).map_err(|_| format!("{url} is not text"))
}

pub(crate) fn get(url: &str) -> Result<Vec<u8>, String> {
    let mut response = ureq::get(url).call().map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}

/// Download one release asset and check it against its line in
/// `sha256sums.txt`; an asset the file does not list is refused.
pub(crate) fn get_verified(base: &str, name: &str, sums: &str) -> Result<Vec<u8>, String> {
    let want = checksum_for(sums, name)
        .ok_or_else(|| format!("the release's sha256sums.txt lists no {name}"))?;
    let url = format!("{base}/{name}");
    let bytes = get(&url).map_err(|e| format!("cannot download {url}: {e}"))?;
    let got = hex(&Sha256::digest(&bytes));
    if !got.eq_ignore_ascii_case(want) {
        return Err(format!(
            "{name} does not match the release's checksum (expected {want}, got {got})"
        ));
    }
    Ok(bytes)
}

/// The digest `sha256sum` wrote for `name`: `<hex>  <name>`, or `<hex> *<name>`
/// in binary mode.
pub(crate) fn checksum_for<'a>(sums: &'a str, name: &str) -> Option<&'a str> {
    sums.lines().find_map(|line| {
        let (digest, file) = line.split_once(char::is_whitespace)?;
        let file = file.trim_start().trim_start_matches('*');
        (file == name).then_some(digest)
    })
}

/// [`get_verified`] for an asset too large to hold in memory: streamed into
/// `path`, hashed on the way.
pub(crate) fn download_verified(
    base: &str,
    name: &str,
    sums: &str,
    path: &Path,
) -> Result<(), String> {
    let want = checksum_for(sums, name)
        .ok_or_else(|| format!("the release's sha256sums.txt lists no {name}"))?;
    let url = format!("{base}/{name}");
    let mut response = ureq::get(&url)
        .call()
        .map_err(|e| format!("cannot download {url}: {e}"))?;
    let mut reader = response.body_mut().as_reader();
    let mut file =
        std::fs::File::create(path).map_err(|e| format!("create {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("cannot download {url}: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        std::io::Write::write_all(&mut file, &buf[..n])
            .map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    let got = hex(&hasher.finalize());
    if !got.eq_ignore_ascii_case(want) {
        return Err(format!(
            "{name} does not match the release's checksum (expected {want}, got {got})"
        ));
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// How a downloaded archive is packed.
#[derive(Clone, Copy)]
pub(crate) enum Packing {
    TarZstd,
    TarGz,
    Zip,
}

/// Download `name` from the release and unpack it into `dir`, unless a
/// finished unpack is there already.
pub(crate) fn fetch_archive(
    base: &str,
    name: &str,
    sums: &str,
    packing: Packing,
    dir: &Path,
) -> Result<(), String> {
    if dir.join(COMPLETE).is_file() {
        return Ok(());
    }
    let parent = dir
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", dir.display()))?;
    std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    let download = parent.join(format!(".{name}.{}", std::process::id()));
    let result = download_verified(base, name, sums, &download)
        .and_then(|()| unpack(&download, packing, dir));
    let _ = std::fs::remove_file(&download);
    result
}

/// Unpack beside `dir` and move it into place, so a concurrent build never
/// sees half of it and an interrupted one leaves nothing that looks finished.
fn unpack(archive: &Path, packing: Packing, dir: &Path) -> Result<(), String> {
    let parent = dir
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", dir.display()))?;
    let staging = parent.join(format!(".unpack-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    let file =
        std::fs::File::open(archive).map_err(|e| format!("open {}: {e}", archive.display()))?;
    let reader = std::io::BufReader::new(file);
    match packing {
        Packing::TarZstd => {
            let decoder = ruzstd::decoding::StreamingDecoder::new(reader)
                .map_err(|e| format!("{} is not zstd: {e}", archive.display()))?;
            tar::Archive::new(decoder)
                .unpack(&staging)
                .map_err(|e| format!("cannot unpack {}: {e}", archive.display()))?;
        }
        Packing::TarGz => {
            tar::Archive::new(flate2::read::GzDecoder::new(reader))
                .unpack(&staging)
                .map_err(|e| format!("cannot unpack {}: {e}", archive.display()))?;
        }
        Packing::Zip => {
            zip::ZipArchive::new(reader)
                .and_then(|mut z| z.extract(&staging))
                .map_err(|e| format!("cannot unpack {}: {e}", archive.display()))?;
        }
    }
    std::fs::write(staging.join(COMPLETE), b"")
        .map_err(|e| format!("write {}: {e}", staging.display()))?;
    if dir.join(COMPLETE).is_file() {
        // Another build finished the same unpack while this one downloaded.
        let _ = std::fs::remove_dir_all(&staging);
        return Ok(());
    }
    // What is there is an earlier attempt that never finished.
    let _ = std::fs::remove_dir_all(dir);
    if let Err(e) = std::fs::rename(&staging, dir) {
        // Another build finished the same unpack first; theirs is as good.
        let _ = std::fs::remove_dir_all(&staging);
        if !dir.join(COMPLETE).is_file() {
            return Err(format!("move the download into {}: {e}", dir.display()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_checksum_line_names_its_file() {
        let sums = "abc  lumen-linux-x86_64.tar.gz\ndef *lumenui-engine-linux-x86_64.json\n";
        assert_eq!(checksum_for(sums, "lumen-linux-x86_64.tar.gz"), Some("abc"));
        assert_eq!(
            checksum_for(sums, "lumenui-engine-linux-x86_64.json"),
            Some("def")
        );
        assert_eq!(checksum_for(sums, "missing"), None);
    }
}
