//! Keeps the templates the toolchain ships and the gallery in step.
//!
//! `scaffold::TEMPLATES` says which templates exist and what each is for; the
//! files come from the payload a release packages beside lumenc, which
//! `tools/fetch-templates.sh` downloads from the template repositories. The
//! two halves are written in different places and nothing links them at
//! compile time, so a template named in one and missing from the other only
//! shows up when someone scaffolds it. These read both sides and compare.
//!
//! Without the payload there is nothing to compare, which is the normal state
//! of a checkout that has not run the script, so each case says what it needs
//! and returns. CI fetches before it tests.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use lumenc::scaffold::{self, TEMPLATES};

/// The payload directory, or nothing when it has not been downloaded.
fn payload() -> Option<PathBuf> {
    match scaffold::payload_dir() {
        Ok(dir) => Some(dir),
        Err(why) => {
            eprintln!("skipping: {why}");
            None
        }
    }
}

/// Every file under `dir`, as slash-joined paths relative to it.
fn files_under(dir: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        let entries =
            std::fs::read_dir(&cur).unwrap_or_else(|e| panic!("reading {}: {e}", cur.display()));
        for entry in entries {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let rel = path
                    .strip_prefix(dir)
                    .expect("walked path is under the root");
                out.insert(
                    rel.components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join("/"),
                );
            }
        }
    }
    out
}

#[test]
fn the_payload_carries_the_gallery_and_nothing_else() {
    let Some(dir) = payload() else { return };

    let mut on_disk = BTreeSet::new();
    for entry in std::fs::read_dir(&dir).expect("reading the template payload") {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            on_disk.insert(path.file_name().unwrap().to_string_lossy().into_owned());
        }
    }
    let registered: BTreeSet<String> = TEMPLATES.iter().map(|t| t.name.to_string()).collect();
    assert_eq!(
        on_disk,
        registered,
        "the templates in {} are not the gallery in scaffold::TEMPLATES. \
         tools/fetch-templates.sh downloads one repository per gallery entry, \
         so a name added to one belongs in the other",
        dir.display()
    );
}

#[test]
fn every_template_carries_a_lumen_toml_a_readme_and_an_entry() {
    if payload().is_none() {
        return;
    }
    for t in TEMPLATES {
        let dir = scaffold::template_dir(t.name).expect("the payload carries the whole gallery");
        let files = files_under(&dir);
        for want in ["lumen.toml", "README.md", "src/main.lmn"] {
            assert!(
                files.contains(want),
                "the `{}` template carries no {want}, so `lumenc new` writes an \
                 app that will not run",
                t.name
            );
        }
    }
}

/// Scaffolding writes the template's whole tree, subdirectories included.
///
/// The copy walks the payload rather than a list of paths, so a template that
/// grows a directory is scaffolded whole with nothing added here; what would
/// go wrong instead is a walk that stops at the top level and drops `src/`.
#[test]
fn scaffolding_writes_every_file_the_template_carries() {
    if payload().is_none() {
        return;
    }
    let dest = std::env::temp_dir().join(format!("lumenc-scaffold-copy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dest);

    for t in TEMPLATES {
        let into = dest.join(t.name);
        let written = scaffold::write_template(t.name, &into)
            .unwrap_or_else(|e| panic!("scaffolding `{}`: {e}", t.name));
        let source = scaffold::template_dir(t.name).expect("the template that was just copied");
        let expected: Vec<String> = files_under(&source).into_iter().collect();
        assert_eq!(
            written, expected,
            "`{}` scaffolded a different file set than it carries",
            t.name
        );
        assert_eq!(
            files_under(&into),
            files_under(&source),
            "the `{}` files on disk after scaffolding are not the ones it carries",
            t.name
        );
    }

    let _ = std::fs::remove_dir_all(&dest);
}

/// A template that is not in the gallery is refused before anything is
/// written, and so is the whole command when the payload is missing.
#[test]
fn a_name_outside_the_gallery_scaffolds_nothing() {
    let dest = std::env::temp_dir().join(format!("lumenc-scaffold-unknown-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dest);

    let error = scaffold::write_template("telepathy", &dest)
        .expect_err("`telepathy` is not a template the gallery lists");
    assert!(
        !dest.exists(),
        "a template that could not be read still created {}",
        dest.display()
    );
    let message = error.to_string();
    assert!(
        message.contains("telepathy") || message.contains("tools/fetch-templates.sh"),
        "the error names the template or how to get the templates, and reads {message}"
    );
}
