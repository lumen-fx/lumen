//! The transfer itself: one URL streamed onto one path, on a plain
//! [`Path`] with no script or plugin state around it.
//!
//! Three rules shape it:
//!
//! - **The destination never holds a half file.** Bytes land in a sibling temp
//!   file and are renamed into place only once the body has finished and the
//!   checksum has verified. Any failure removes the temp and leaves whatever
//!   was at the destination untouched.
//! - **One pass.** The hash is computed while the bytes are written, so
//!   verifying a checksum costs no second read of the file.
//! - **A status is an outcome.** A reply that is not 2xx fails the transfer.
//!   You asked for a file and did not get one, so there is nothing to write.

use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};
use ureq::Agent;

/// How much is read from the socket before the progress callback is offered
/// another figure.
const CHUNK: usize = 64 * 1024;

/// What a transfer reports when it could not do what was asked: the line an
/// author reads, without the `lumen-download: ` prefix.
pub type Failure = String;

/// The digest a finished file has to match, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checksum {
    /// Take whatever arrives.
    None,
    /// The file has to hash to these 32 bytes.
    Sha256([u8; 32]),
}

/// The bounds a transfer runs under, from the module's `config` table.
#[derive(Debug, Clone, Copy, Default)]
pub struct Limits {
    /// How long a stalled connection has to produce the response line and
    /// headers before the transfer gives up. `None` waits indefinitely.
    pub timeout_ms: Option<u64>,
    /// The largest body accepted, in bytes. `None` accepts any size.
    pub max_bytes: Option<u64>,
}

/// What a finished transfer wrote.
#[derive(Debug, Clone)]
pub struct Transferred {
    /// The file, at the path it was asked for.
    pub path: PathBuf,
    /// How many bytes arrived.
    pub received: u64,
    /// What the server said would arrive, when it said.
    pub total: Option<u64>,
}

/// Read a checksum as a script spelled it.
///
/// Three spellings are accepted: the empty string (no checking),
/// `sha256:<64 hex digits>`, and a bare 64-digit hex string, which is read as
/// sha256 because that is the only digest this module computes. The prefix and
/// the digits are matched without regard to case. Anything else is refused
/// rather than guessed at, so a truncated or mistyped digest fails the call
/// instead of silently checking nothing.
pub fn parse_checksum(spec: &str) -> Result<Checksum, Failure> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Ok(Checksum::None);
    }
    let digits = match spec.split_once(':') {
        Some((algo, rest)) if algo.eq_ignore_ascii_case("sha256") => rest.trim(),
        Some((algo, _)) => {
            return Err(format!(
                "unsupported checksum format: `{algo}` is not an algorithm this module computes; \
                 write `sha256:<64 hex digits>`"
            ));
        }
        None => spec,
    };
    if digits.len() != 64 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "unsupported checksum format: `{spec}` is not 64 hex digits; write \
             `sha256:<64 hex digits>`"
        ));
    }
    let mut bytes = [0u8; 32];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&digits[i * 2..i * 2 + 2], 16)
            .map_err(|_| format!("unsupported checksum format: `{spec}` is not hexadecimal"))?;
    }
    Ok(Checksum::Sha256(bytes))
}

/// The lowercase hex spelling of a digest, as a checksum is written.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Stream `url` onto `dest`, hashing as it goes.
///
/// `progress` is offered the running byte count and the size the server
/// declared, as often as bytes arrive; throttling it is the caller's business.
/// The destination is replaced only by a complete, verified file.
pub fn to_file(
    url: &str,
    dest: &Path,
    checksum: &Checksum,
    limits: &Limits,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<Transferred, Failure> {
    let dir = match dest.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let temp = temp_path(dest, &dir)?;

    let outcome = stream(url, &temp, checksum, limits, progress);
    match outcome {
        Ok((received, total)) => match fs::rename(&temp, dest) {
            Ok(()) => Ok(Transferred {
                path: dest.to_path_buf(),
                received,
                total,
            }),
            Err(e) => {
                let _ = fs::remove_file(&temp);
                Err(format!("{}: {e}", dest.display()))
            }
        },
        Err(failure) => {
            let _ = fs::remove_file(&temp);
            Err(failure)
        }
    }
}

/// The sibling the bytes land in first. A per-process counter keeps two
/// transfers racing the same destination off each other's file.
fn temp_path(dest: &Path, dir: &Path) -> Result<PathBuf, Failure> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let name = dest
        .file_name()
        .ok_or_else(|| format!("{}: the path names no file", dest.display()))?;
    Ok(dir.join(format!(
        ".{}.part-{}-{}",
        name.to_string_lossy(),
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    )))
}

/// Everything between opening the connection and a finished, verified temp
/// file. The caller owns the temp file's fate either way.
fn stream(
    url: &str,
    temp: &Path,
    checksum: &Checksum,
    limits: &Limits,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<(u64, Option<u64>), Failure> {
    // The timeouts cover getting a reply started, not carrying it: a deadline
    // over the whole body would kill exactly the large transfers this module
    // exists for, and ureq exposes no idle-socket deadline that would bound a
    // stall without bounding the transfer. Redirects (ten deep) and TLS
    // (rustls over the web-PKI roots) are ureq's defaults.
    let timeout = limits.timeout_ms.map(Duration::from_millis);
    let config = Agent::config_builder()
        .http_status_as_error(false)
        .timeout_resolve(timeout)
        .timeout_connect(timeout)
        .timeout_recv_response(timeout)
        .build();
    let agent: Agent = config.into();

    let mut reply = agent.get(url).call().map_err(|e| format!("{url}: {e}"))?;
    let status = reply.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(format!("{url}: the server answered HTTP {status}"));
    }
    let total = reply.body().content_length();
    if let (Some(total), Some(max)) = (total, limits.max_bytes)
        && total > max
    {
        return Err(format!(
            "{url}: the server declared {total} bytes and the limit is {max}; raise `max_bytes` \
             in the module's config to accept it"
        ));
    }

    let file = File::create(temp).map_err(|e| format!("{}: {e}", temp.display()))?;
    let mut writer = BufWriter::new(file);
    let mut reader = reply.body_mut().as_reader();
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    let mut received: u64 = 0;

    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("{url}: {e} after {received} bytes"))?;
        if n == 0 {
            break;
        }
        received += n as u64;
        if let Some(max) = limits.max_bytes
            && received > max
        {
            return Err(format!(
                "{url}: the body passed the {max} byte limit; raise `max_bytes` in the module's \
                 config to accept it"
            ));
        }
        hasher.update(&buf[..n]);
        writer
            .write_all(&buf[..n])
            .map_err(|e| format!("{}: {e}", temp.display()))?;
        progress(received, total);
    }

    // Checked before the file is put on disk for good: a body that hashes to
    // something else is going to be thrown away, and there is no reason to pay
    // for its durability first.
    if let Checksum::Sha256(expected) = checksum {
        let actual = hasher.finalize();
        if actual.as_slice() != expected.as_slice() {
            return Err(format!(
                "{url}: checksum mismatch; expected sha256:{}, got sha256:{}",
                hex(expected.as_slice()),
                hex(actual.as_slice())
            ));
        }
    }

    // On disk before the rename, so a machine that loses power mid-transfer
    // finds either the old file at the destination or none, never a name
    // promising bytes that were still in a buffer.
    writer
        .into_inner()
        .map_err(|e| format!("{}: {e}", temp.display()))?
        .sync_all()
        .map_err(|e| format!("{}: {e}", temp.display()))?;

    Ok((received, total))
}
