//! Records the link command a build runs, then runs it.
//!
//! `rustc -Clinker=link-recorder` puts this binary where the linker goes. It
//! writes one JSON line per link to `$LUMEN_LINK_RECORD`, copies every input
//! file the line names into `$LUMEN_LINK_STAGE`, and then runs the linker
//! named by `$LUMEN_REAL_LINKER` (or the first entry of the path-separated
//! `$LUMEN_REAL_LINKER_HINT` list that exists) with the arguments it was
//! given, exiting with that linker's status. With neither of the first two
//! variables set it records nothing and only runs the linker.
//! `$LUMEN_LINK_RSP_STYLE` is `gnu` or `msvc`, for a toolchain whose response
//! files are in neither of the two shapes rustc writes.
//!
//! Staging is the reason this exists rather than `rustc --print link-args`.
//! Half the inputs of a link are temporary files rustc deletes the moment the
//! linker returns - the per-codegen-unit `*.rcgu.o` objects and `symbols.o` -
//! so a line captured after the fact names files that no longer exist. The
//! only moment they can be copied is while the link is running.
//!
//! `crates/modules/src/link_kit.rs` documents the record's fields and holds
//! the reader; `lumenc link-kit emit` turns a record plus a stage directory
//! into a shippable kit.

#![forbid(unsafe_code)]

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use sha2::{Digest, Sha256};

fn main() -> ExitCode {
    // Lossy for the record, exact for the run: the record is JSON and the
    // real linker must receive what rustc wrote, byte for byte.
    let raw: Vec<OsString> = env::args_os().skip(1).collect();
    let text: Vec<String> = raw
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    if let Err(e) = record(&text) {
        // A half-staged kit links binaries that cannot be reproduced, so a
        // staging failure fails the build rather than the kit.
        eprintln!("link-recorder: {e}");
        return ExitCode::from(1);
    }

    let linker = match real_linker() {
        Ok(linker) => linker,
        Err(e) => {
            eprintln!("link-recorder: {e}");
            return ExitCode::from(127);
        }
    };
    match Command::new(&linker).args(&raw).status() {
        Ok(status) => ExitCode::from(u8::try_from(status.code().unwrap_or(1)).unwrap_or(1)),
        Err(e) => {
            eprintln!(
                "link-recorder: cannot run the linker `{}`: {e}",
                linker.to_string_lossy()
            );
            ExitCode::from(127)
        }
    }
}

/// The linker to run once the line is recorded.
///
/// `LUMEN_REAL_LINKER` names it outright. `LUMEN_REAL_LINKER_HINT` is a list
/// of candidate paths in the platform's `PATH` syntax, for a caller that
/// knows where a linker might be but not which one is installed; the first
/// entry that is a file wins. Neither set is an error rather than a guess: a
/// guess would silently link with a different linker than the one the record
/// says produced the binary.
fn real_linker() -> Result<OsString, String> {
    if let Some(named) = env::var_os("LUMEN_REAL_LINKER").filter(|v| !v.is_empty()) {
        return Ok(named);
    }
    let hints = env::var_os("LUMEN_REAL_LINKER_HINT").unwrap_or_default();
    for hint in env::split_paths(&hints) {
        if hint.is_file() {
            return Ok(hint.into_os_string());
        }
    }
    if hints.is_empty() {
        return Err(
            "no linker to run. Set LUMEN_REAL_LINKER to the real linker, or \
                    LUMEN_REAL_LINKER_HINT to a path-separated list of candidates."
                .to_string(),
        );
    }
    Err(format!(
        "no entry of LUMEN_REAL_LINKER_HINT exists: {}",
        hints.to_string_lossy()
    ))
}

/// Stage the link's inputs and append its record, if a caller asked for one.
fn record(args: &[String]) -> Result<(), String> {
    let (Some(record_path), Some(stage)) = (
        env::var_os("LUMEN_LINK_RECORD").filter(|v| !v.is_empty()),
        env::var_os("LUMEN_LINK_STAGE").filter(|v| !v.is_empty()),
    ) else {
        return Ok(());
    };
    let stage = PathBuf::from(stage);
    fs::create_dir_all(&stage).map_err(|e| format!("cannot create {}: {e}", stage.display()))?;

    let argv = expand(args, 0)?;
    // The output of a rebuild is a file that already exists, and staging it
    // would copy the last binary into the kit for nothing.
    let out_at = argv.iter().position(|a| a == "-o").map(|i| i + 1);
    let mut staged = Vec::with_capacity(argv.len());
    for (i, arg) in argv.iter().enumerate() {
        let name = if Some(i) == out_at {
            None
        } else {
            stage_input(arg, &stage)?
        };
        staged.push(name.unwrap_or_else(|| arg.clone()));
    }

    let cwd = env::current_dir().unwrap_or_default();
    let line = serde_json::json!({
        "out": output_of(&argv),
        "argv": argv,
        "staged_argv": staged,
        "cwd": cwd.to_string_lossy(),
        // The MSVC linker resolves a bare `foo.lib` through this variable, so
        // a Windows record that dropped it would name libraries the replay
        // cannot find.
        "env": { "LIB": env::var("LIB").ok() },
    });
    let mut line = serde_json::to_string(&line).map_err(|e| format!("cannot encode: {e}"))?;
    line.push('\n');

    // Cargo runs links in parallel, so several recorders append here at once.
    // One `write_all` of the whole line is what keeps them apart: a write to
    // a regular file opened for append is serialized against other appends.
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&record_path)
        .map_err(|e| format!("cannot open {}: {e}", record_path.to_string_lossy()))?;
    file.write_all(line.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", record_path.to_string_lossy()))
}

/// The `-o` / `/OUT:` value, which is how a reader tells one link from the
/// several a build runs.
fn output_of(argv: &[String]) -> Option<String> {
    let mut it = argv.iter();
    while let Some(arg) = it.next() {
        if arg == "-o" {
            return it.next().cloned();
        }
        let head: String = arg.chars().take(5).collect();
        if head.eq_ignore_ascii_case("/out:") || head.eq_ignore_ascii_case("-out:") {
            return Some(arg[5..].to_string());
        }
    }
    None
}

/// Split an argument that carries a path into the part that stays and the
/// path that gets staged.
///
/// These are the shapes rustc uses to hand a linker the list of symbols a
/// library exports: a version script on the GNU linkers, an exported-symbols
/// list on ld64, a module-definition file on MSVC. Each is generated per link
/// and deleted with the temporary directory that holds it.
fn split_path_flag(arg: &str) -> Option<(&str, &str)> {
    const PREFIXES: &[&str] = &[
        "-Wl,--version-script=",
        "-Wl,--dynamic-list=",
        "-Wl,--retain-symbols-file=",
        "-Wl,-exported_symbols_list,",
        "--version-script=",
        "-exported_symbols_list,",
    ];
    for prefix in PREFIXES {
        if let Some(path) = arg.strip_prefix(prefix) {
            return Some((prefix, path));
        }
    }
    // MSVC spells its flags case-insensitively and with either lead character.
    if arg.len() > 5 {
        let (head, path) = arg.split_at(5);
        if head.eq_ignore_ascii_case("/DEF:") || head.eq_ignore_ascii_case("-DEF:") {
            return Some((head, path));
        }
    }
    None
}

/// Copy `arg` into the stage directory if it names a file, and answer the
/// name it was staged under.
///
/// The name is the first four bytes of the content hash and the original file
/// name. The hash is what lets the links of one build share staged copies of
/// the rlibs they all read, and it keeps two different files that happen to
/// share a name apart.
fn stage_input(arg: &str, stage: &Path) -> Result<Option<String>, String> {
    // A few flags carry a path rather than being one. rustc writes the export
    // list of a library target into the temporary directory it deletes when
    // the link returns, so a record that kept the original path names a file
    // that is already gone, and a replay without the list exports a different
    // set of symbols than the build shipped.
    if let Some((flag, path)) = split_path_flag(arg) {
        return match stage_input(path, stage)? {
            Some(name) => Ok(Some(format!("{flag}{name}"))),
            None => Ok(None),
        };
    }
    let src = Path::new(arg);
    // A flag is never a file, and checking the file system for every one of
    // them is the bulk of what this shim does per link.
    if arg.starts_with('-') || (arg.starts_with('/') && !src.is_absolute()) {
        return Ok(None);
    }
    if !src.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(src).map_err(|e| format!("cannot read {arg}: {e}"))?;
    let digest = Sha256::digest(&bytes);
    let base = src
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unnamed".to_string());
    let name = format!(
        "{:02x}{:02x}{:02x}{:02x}-{base}",
        digest[0], digest[1], digest[2], digest[3]
    );

    let dst = stage.join(&name);
    if !dst.exists() {
        // Written aside and renamed into place: a reader of the stage
        // directory must never see a file another recorder is still writing.
        let tmp = stage.join(format!("{name}.{}.part", std::process::id()));
        fs::write(&tmp, &bytes).map_err(|e| format!("cannot stage {arg}: {e}"))?;
        if fs::rename(&tmp, &dst).is_err() {
            // Another recorder staged the same content first, which is the
            // outcome either way.
            let _ = fs::remove_file(&tmp);
        }
    }
    Ok(Some(name))
}

/// Replace every `@file` argument with the arguments the file holds.
///
/// rustc hands the MSVC linker its whole line this way, so a recorder that
/// did not expand would record one argument and stage nothing.
fn expand(args: &[String], depth: u32) -> Result<Vec<String>, String> {
    if depth > 8 {
        return Err("response files nest more than eight deep".to_string());
    }
    let mut out = Vec::with_capacity(args.len());
    for arg in args {
        let Some(path) = arg.strip_prefix('@').filter(|p| Path::new(p).is_file()) else {
            out.push(arg.clone());
            continue;
        };
        let bytes = fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))?;
        out.extend(expand(&parse_response(&bytes), depth + 1)?);
    }
    Ok(out)
}

/// Tokenize a response file.
///
/// The byte-order mark picks the encoding and, with it, the quoting rules:
/// rustc writes the MSVC linker a UTF-16LE file whose paths are full of
/// backslashes, so a backslash there is a path separator rather than an
/// escape, and it writes the GNU linkers plain bytes with one argument per
/// line and a backslash escaping the character after it.
/// `LUMEN_LINK_RSP_STYLE=gnu|msvc` settles it for a toolchain that writes
/// neither shape.
fn parse_response(bytes: &[u8]) -> Vec<String> {
    let (text, utf16) = if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        (String::from_utf16_lossy(&units), true)
    } else {
        let stripped = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
        (String::from_utf8_lossy(stripped).into_owned(), false)
    };
    let msvc = match env::var("LUMEN_LINK_RSP_STYLE").as_deref() {
        Ok("msvc") => true,
        Ok("gnu") => false,
        _ => utf16,
    };
    if msvc {
        tokenize_msvc(&text)
    } else {
        tokenize_gnu(&text)
    }
}

/// MSVC rules: whitespace separates, a double quote groups, and a backslash
/// is an ordinary character.
fn tokenize_msvc(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut token = String::new();
    let mut started = false;
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    out.push(std::mem::take(&mut token));
                    started = false;
                }
            }
            c => {
                token.push(c);
                started = true;
            }
        }
    }
    if started {
        out.push(token);
    }
    out
}

/// GNU rules, which is the shape rustc writes for every linker but MSVC's:
/// one argument per line, optionally wrapped in double quotes, with a
/// backslash escaping the character after it. Line-based rather than
/// whitespace-based, so a path with a space in it stays one argument.
fn tokenize_gnu(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let inner = match (line.strip_prefix('"'), line.strip_suffix('"')) {
            (Some(_), Some(_)) if line.len() >= 2 => &line[1..line.len() - 1],
            _ => line,
        };
        let mut token = String::with_capacity(inner.len());
        let mut escaped = false;
        for c in inner.chars() {
            if escaped {
                token.push(c);
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else {
                token.push(c);
            }
        }
        out.push(token);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{output_of, parse_response, split_path_flag, tokenize_gnu, tokenize_msvc};

    fn utf16le(text: &str) -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xFE];
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn an_export_list_is_a_path_to_stage_rather_than_a_flag() {
        assert_eq!(
            split_path_flag("-Wl,--version-script=/tmp/rustcAbC/list"),
            Some(("-Wl,--version-script=", "/tmp/rustcAbC/list"))
        );
        assert_eq!(
            split_path_flag("-Wl,-exported_symbols_list,/tmp/rustcAbC/list"),
            Some(("-Wl,-exported_symbols_list,", "/tmp/rustcAbC/list"))
        );
        assert_eq!(
            split_path_flag("/def:C:\\t\\lib.def"),
            Some(("/def:", "C:\\t\\lib.def"))
        );
    }

    #[test]
    fn an_ordinary_flag_carries_no_path() {
        assert_eq!(split_path_flag("-Wl,--gc-sections"), None);
        assert_eq!(split_path_flag("-lgtk-3"), None);
        assert_eq!(split_path_flag("/DEBUG"), None);
    }

    #[test]
    fn a_backslash_survives_the_msvc_rules() {
        assert_eq!(
            tokenize_msvc("/OUT:C:\\a\\b.exe C:\\lib\\std.rlib"),
            vec!["/OUT:C:\\a\\b.exe", "C:\\lib\\std.rlib"]
        );
    }

    #[test]
    fn a_quote_groups_a_path_with_a_space() {
        assert_eq!(
            tokenize_msvc("\"C:\\Program Files\\x.lib\" /DEBUG"),
            vec!["C:\\Program Files\\x.lib", "/DEBUG"]
        );
    }

    #[test]
    fn an_empty_quoted_argument_is_still_an_argument() {
        assert_eq!(tokenize_msvc("-a \"\" -b"), vec!["-a", "", "-b"]);
    }

    #[test]
    fn newlines_separate_arguments_like_spaces() {
        assert_eq!(tokenize_msvc("-a\r\n-b\n-c"), vec!["-a", "-b", "-c"]);
    }

    #[test]
    fn the_gnu_rules_take_one_argument_per_line() {
        assert_eq!(
            tokenize_gnu("/tmp/a\\ b.o\n-lm\n\"one two\"\n"),
            vec!["/tmp/a b.o", "-lm", "one two"]
        );
        // A blank line is spacing rather than an empty argument.
        assert_eq!(tokenize_gnu("-a\n\n-b\n"), vec!["-a", "-b"]);
    }

    #[test]
    fn a_utf16_response_file_reads_as_msvc_whatever_the_style_says() {
        assert_eq!(
            parse_response(&utf16le("/OUT:x.exe a\\b.o")),
            vec!["/OUT:x.exe", "a\\b.o"]
        );
    }

    #[test]
    fn a_utf8_response_file_reads_with_or_without_a_mark() {
        let mut marked = vec![0xEF, 0xBB, 0xBF];
        marked.extend_from_slice(b"-o\nout\na.o\n");
        assert_eq!(parse_response(&marked), vec!["-o", "out", "a.o"]);
        assert_eq!(parse_response(b"-o\nout\na.o\n"), vec!["-o", "out", "a.o"]);
    }

    #[test]
    fn the_output_is_read_from_either_spelling() {
        assert_eq!(
            output_of(&["-a".into(), "-o".into(), "app".into()]),
            Some("app".to_string())
        );
        assert_eq!(
            output_of(&["/OUT:app.exe".into(), "x.o".into()]),
            Some("app.exe".to_string())
        );
        assert_eq!(output_of(&["x.o".into()]), None);
    }
}
