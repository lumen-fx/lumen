//! `lumenc add`, `remove`, `fetch`, and `update`: the app's registry
//! dependencies, from the command line.
//!
//! `add` and `remove` edit `lumen.toml` and `fetch` and `update` drive
//! [`crate::lpm`]; between them they cover what an author would otherwise do
//! by hand. The edits go through `toml_edit`, so an author's comments,
//! spacing, and key order survive.
//!
//! The compile paths resolve on their own, so none of this is a step anyone
//! has to remember: `fetch` exists for a CI job that wants the download to be
//! its own cacheable step, and `update` for moving a pin deliberately.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use toml_edit::{DocumentMut, Item, Table, Value};

use crate::lpm;

/// `lumenc add <name>[@<req>] [--plugin] [--config k=v]...`
pub fn cmd_add(args: impl Iterator<Item = String>) -> ExitCode {
    const ADD_USAGE: &str = "lumenc add - declare a registry package

USAGE:
    lumenc add <name>[@<req>] [<dir>] [--plugin] [--config <k>=<v>]...

Writes the package into <dir>/lumen.toml and resolves it, so the app is ready
to run. <dir> defaults to the current directory.

    --plugin          Declare a compiler plugin under [[plugins]] instead of
                      a runtime dependency under [dependencies].
    --config k=v      A key in the package's own `config` table. The value is
                      read as TOML, so `true`, `7`, and `\"mm\"` keep their
                      types; anything else is taken as a string. Repeatable.

With no <req>, the newest version the registry publishes is resolved and
written as the requirement.";

    let mut spec: Option<String> = None;
    let mut dir: Option<String> = None;
    let mut plugin = false;
    let mut config: Vec<(String, String)> = Vec::new();
    let mut args = args;
    while let Some(a) = args.next() {
        match a.as_str() {
            h if crate::is_help_flag(h) => {
                println!("{ADD_USAGE}");
                return ExitCode::SUCCESS;
            }
            "--plugin" => plugin = true,
            s if s == "--config" || s.starts_with("--config=") => {
                let v = match s.strip_prefix("--config=") {
                    Some(v) => Some(v.to_string()),
                    None => args.next(),
                };
                match v.as_deref().and_then(|v| v.split_once('=')) {
                    Some((k, v)) if !k.trim().is_empty() => {
                        config.push((k.trim().to_string(), v.to_string()));
                    }
                    _ => {
                        eprintln!("lumenc add: --config needs <key>=<value>");
                        return ExitCode::from(2);
                    }
                }
            }
            _ if spec.is_none() => spec = Some(a),
            _ if dir.is_none() => dir = Some(a),
            _ => {
                eprintln!("lumenc add: unexpected argument '{a}'\n\n{ADD_USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let Some(spec) = spec else {
        eprintln!("lumenc add: missing <name>\n\n{ADD_USAGE}");
        return ExitCode::from(2);
    };
    let (name, req) = match spec.split_once('@') {
        Some((name, req)) if !name.is_empty() && !req.is_empty() => {
            (name.to_string(), Some(req.to_string()))
        }
        Some(_) => {
            eprintln!("lumenc add: '{spec}' is not <name>@<req>");
            return ExitCode::from(2);
        }
        None => (spec.clone(), None),
    };

    let dir = PathBuf::from(dir.unwrap_or_else(|| ".".to_string()));
    let path = dir.join("lumen.toml");
    let mut doc = match read_document(&path) {
        Ok(doc) => doc,
        Err(e) => {
            eprintln!("lumenc add: {e}");
            return ExitCode::from(2);
        }
    };

    // An unpinned add resolves first and writes what it got, so the file
    // records a requirement rather than a wildcard nobody chose.
    let written = req.clone().unwrap_or_else(|| "*".to_string());
    if let Err(e) = declare(&mut doc, &name, &written, plugin, &config) {
        eprintln!("lumenc add: {e}");
        return ExitCode::from(2);
    }
    if let Err(e) = write_document(&path, &doc) {
        eprintln!("lumenc add: {e}");
        return ExitCode::FAILURE;
    }

    let resolved = match resolve(&dir, lpm::Mode::of_invocation()) {
        Ok(resolved) => resolved,
        Err(e) => {
            eprintln!("lumenc add: {e}");
            return ExitCode::FAILURE;
        }
    };
    if req.is_none() {
        let Some(version) = resolved.versions.get(&name).cloned() else {
            eprintln!("lumenc add: the registry answered with no package called '{name}'");
            return ExitCode::FAILURE;
        };
        if let Err(e) = declare(&mut doc, &name, &version, plugin, &config)
            .and_then(|()| write_document(&path, &doc))
        {
            eprintln!("lumenc add: {e}");
            return ExitCode::FAILURE;
        }
        println!("lumenc add: {name} {version}");
    } else {
        println!("lumenc add: {name} {written}");
    }
    ExitCode::SUCCESS
}

/// `lumenc remove <name>`
pub fn cmd_remove(args: impl Iterator<Item = String>) -> ExitCode {
    const REMOVE_USAGE: &str = "lumenc remove - drop a declared package

USAGE:
    lumenc remove <name> [<dir>]

Deletes the package's [dependencies] entry or its [[plugins]] entry from
<dir>/lumen.toml and re-resolves what is left. <dir> defaults to the current
directory.";

    let mut name: Option<String> = None;
    let mut dir: Option<String> = None;
    for a in args {
        match a.as_str() {
            h if crate::is_help_flag(h) => {
                println!("{REMOVE_USAGE}");
                return ExitCode::SUCCESS;
            }
            _ if name.is_none() => name = Some(a),
            _ if dir.is_none() => dir = Some(a),
            _ => {
                eprintln!("lumenc remove: unexpected argument '{a}'\n\n{REMOVE_USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let Some(name) = name else {
        eprintln!("lumenc remove: missing <name>\n\n{REMOVE_USAGE}");
        return ExitCode::from(2);
    };
    let dir = PathBuf::from(dir.unwrap_or_else(|| ".".to_string()));
    let path = dir.join("lumen.toml");
    let mut doc = match read_document(&path) {
        Ok(doc) => doc,
        Err(e) => {
            eprintln!("lumenc remove: {e}");
            return ExitCode::from(2);
        }
    };
    if !undeclare(&mut doc, &name) {
        eprintln!("lumenc remove: {} declares no '{name}'", path.display());
        return ExitCode::FAILURE;
    }
    if let Err(e) = write_document(&path, &doc) {
        eprintln!("lumenc remove: {e}");
        return ExitCode::FAILURE;
    }
    if let Err(e) = resolve(&dir, lpm::Mode::of_invocation()) {
        eprintln!("lumenc remove: {e}");
        return ExitCode::FAILURE;
    }
    println!("lumenc remove: {name}");
    ExitCode::SUCCESS
}

/// `lumenc fetch [<dir>] [--locked] [--target <t>] [--offline]`
pub fn cmd_fetch(args: impl Iterator<Item = String>) -> ExitCode {
    const FETCH_USAGE: &str = "lumenc fetch - download the app's registry packages

USAGE:
    lumenc fetch [<dir>] [--locked] [--target <target>] [--offline]

Resolves every `version` source the app declares and downloads what they
resolve to, writing lumen.lock. Every compile path does this on its own; run
it alone when the download should be its own step, as in a CI job that caches
it. <dir> defaults to the current directory.

    --locked          Fail rather than change lumen.lock.
    --target T        Resolve for another platform (linux-x86_64 |
                      linux-aarch64 | macos-x86_64 | macos-aarch64 |
                      windows-x86_64 | windows-aarch64), which is what
                      `lumenc package --target` needs.
    --offline         Use what is already downloaded and never reach the
                      network.";

    let mut dir: Option<String> = None;
    let mut target: Option<String> = None;
    let mut locked = false;
    let mut args = args;
    while let Some(a) = args.next() {
        match a.as_str() {
            h if crate::is_help_flag(h) => {
                println!("{FETCH_USAGE}");
                return ExitCode::SUCCESS;
            }
            "--locked" => locked = true,
            // Stripped before dispatch, like every other resolving command;
            // matched here so the flag is documented where it is used.
            "--offline" => {}
            s if s == "--target" || s.starts_with("--target=") => {
                match s.strip_prefix("--target=") {
                    Some(v) => target = Some(v.to_string()),
                    None => match args.next() {
                        Some(v) => target = Some(v),
                        None => {
                            eprintln!("lumenc fetch: --target needs a platform name");
                            return ExitCode::from(2);
                        }
                    },
                }
            }
            _ if dir.is_none() => dir = Some(a),
            _ => {
                eprintln!("lumenc fetch: unexpected argument '{a}'\n\n{FETCH_USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let dir = PathBuf::from(dir.unwrap_or_else(|| ".".to_string()));
    let mode = lpm::Mode {
        locked,
        offline: lpm::offline(),
    };
    let target = match target {
        Some(name) => match crate::package_cli::Target::parse(&name) {
            Some(target) => target.name(),
            None => {
                eprintln!("lumenc fetch: no target called '{name}'");
                return ExitCode::from(2);
            }
        },
        None => lpm::host_target(),
    };
    let reqs = match crate::registry_requirements(&dir) {
        Ok(reqs) => reqs,
        Err(e) => {
            eprintln!("lumenc fetch: {e}");
            return ExitCode::from(2);
        }
    };
    match lpm::resolve(&dir, target, &reqs, mode) {
        Ok(_) if reqs.is_empty() => {
            println!("lumenc fetch: this app declares no registry packages");
            ExitCode::SUCCESS
        }
        Ok(_) => {
            println!(
                "lumenc fetch: {} package{} for {target}",
                reqs.len(),
                if reqs.len() == 1 { "" } else { "s" }
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("lumenc fetch: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `lumenc update [<name>...]`
pub fn cmd_update(args: impl Iterator<Item = String>) -> ExitCode {
    const UPDATE_USAGE: &str = "lumenc update - move a pinned package forward

USAGE:
    lumenc update [<name>...] [--dir <dir>]

Re-resolves the app's registry packages against what the registry publishes
now and rewrites lumen.lock. With no names every declared package moves as far
as its requirement allows; with names, only those do.

    --dir DIR         The app directory (default: the current one).";

    let mut dir: Option<String> = None;
    let mut names: Vec<String> = Vec::new();
    let mut args = args;
    while let Some(a) = args.next() {
        match a.as_str() {
            h if crate::is_help_flag(h) => {
                println!("{UPDATE_USAGE}");
                return ExitCode::SUCCESS;
            }
            s if s == "--dir" || s.starts_with("--dir=") => match s.strip_prefix("--dir=") {
                Some(v) => dir = Some(v.to_string()),
                None => match args.next() {
                    Some(v) => dir = Some(v),
                    None => {
                        eprintln!("lumenc update: --dir needs a directory");
                        return ExitCode::from(2);
                    }
                },
            },
            s if s.starts_with('-') => {
                eprintln!("lumenc update: unexpected argument '{s}'\n\n{UPDATE_USAGE}");
                return ExitCode::from(2);
            }
            _ => names.push(a),
        }
    }
    let dir = PathBuf::from(dir.unwrap_or_else(|| ".".to_string()));
    let reqs = match crate::registry_requirements(&dir) {
        Ok(reqs) => reqs,
        Err(e) => {
            eprintln!("lumenc update: {e}");
            return ExitCode::from(2);
        }
    };
    if reqs.is_empty() {
        println!("lumenc update: this app declares no registry packages");
        return ExitCode::SUCCESS;
    }
    if let Some(unknown) = names.iter().find(|n| !reqs.iter().any(|r| r.name == **n)) {
        eprintln!("lumenc update: this app declares no '{unknown}'");
        return ExitCode::from(2);
    }
    match lpm::update(
        &dir,
        lpm::host_target(),
        &reqs,
        &names,
        lpm::Mode::of_invocation(),
    ) {
        Ok(_) => {
            println!(
                "lumenc update: wrote {}",
                dir.join(lpm::LOCK_FILE).display()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("lumenc update: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Resolve the app in `dir` for this machine.
fn resolve(dir: &Path, mode: lpm::Mode) -> Result<lpm::Resolved, String> {
    let reqs = crate::registry_requirements(dir)?;
    lpm::resolve(dir, lpm::host_target(), &reqs, mode)
}

// ============================================================
// Editing lumen.toml
// ============================================================

/// Read `lumen.toml`, keeping every byte of formatting. A missing file starts
/// an empty document, so `lumenc add` works in a directory that has none yet.
fn read_document(path: &Path) -> Result<DocumentMut, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    text.parse::<DocumentMut>()
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Write the document back.
fn write_document(path: &Path, doc: &DocumentMut) -> Result<(), String> {
    std::fs::write(path, doc.to_string()).map_err(|e| format!("{}: {e}", path.display()))
}

/// Put `name` in the table `plugin` selects, at requirement `req`, with
/// `config` as its own config table. Declaring a name the other table already
/// holds is refused: one name means one package.
fn declare(
    doc: &mut DocumentMut,
    name: &str,
    req: &str,
    plugin: bool,
    config: &[(String, String)],
) -> Result<(), String> {
    if plugin {
        if doc
            .get("dependencies")
            .and_then(Item::as_table_like)
            .is_some_and(|t| t.contains_key(name))
        {
            return Err(format!(
                "'{name}' is already a [dependencies] entry; remove it first, or drop --plugin"
            ));
        }
        let array = doc
            .entry("plugins")
            .or_insert_with(|| Item::ArrayOfTables(toml_edit::ArrayOfTables::new()))
            .as_array_of_tables_mut()
            .ok_or_else(|| "lumen.toml: [[plugins]] is not an array of tables".to_string())?;
        if !array
            .iter()
            .any(|t| t.get("name").and_then(Item::as_str) == Some(name))
        {
            let mut table = Table::new();
            table["name"] = toml_edit::value(name);
            array.push(table);
        }
        let entry = array
            .iter_mut()
            .find(|t| t.get("name").and_then(Item::as_str) == Some(name))
            .expect("the entry is there or was just pushed");
        entry.remove("path");
        entry["version"] = toml_edit::value(req);
        if !config.is_empty() {
            entry["config"] = toml_edit::value(config_table(config)?);
        }
        return Ok(());
    }

    if doc
        .get("plugins")
        .and_then(Item::as_array_of_tables)
        .is_some_and(|a| {
            a.iter()
                .any(|t| t.get("name").and_then(Item::as_str) == Some(name))
        })
    {
        return Err(format!(
            "'{name}' is already a [[plugins]] entry; remove it first, or pass --plugin"
        ));
    }
    let deps = doc
        .entry("dependencies")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_like_mut()
        .ok_or_else(|| "lumen.toml: [dependencies] is not a table".to_string())?;
    if config.is_empty() {
        deps.insert(name, toml_edit::value(req));
    } else {
        let mut table = toml_edit::InlineTable::new();
        table.insert("version", Value::from(req));
        table.insert("config", Value::InlineTable(config_table(config)?));
        deps.insert(name, Item::Value(Value::InlineTable(table)));
    }
    Ok(())
}

/// Delete `name` from whichever table declares it. `false` when neither does.
fn undeclare(doc: &mut DocumentMut, name: &str) -> bool {
    let mut found = false;
    if let Some(deps) = doc
        .get_mut("dependencies")
        .and_then(Item::as_table_like_mut)
    {
        found |= deps.remove(name).is_some();
    }
    if let Some(array) = doc
        .get_mut("plugins")
        .and_then(Item::as_array_of_tables_mut)
    {
        let before = array.len();
        array.retain(|t| t.get("name").and_then(Item::as_str) != Some(name));
        found |= array.len() != before;
        if array.is_empty() {
            doc.remove("plugins");
        }
    }
    // An emptied table would leave a bare `[dependencies]` header behind,
    // which is a declaration of nothing.
    if doc
        .get("dependencies")
        .and_then(Item::as_table_like)
        .is_some_and(|t| t.is_empty())
    {
        doc.remove("dependencies");
    }
    found
}

/// Read `--config k=v` pairs into a table. A value that parses as TOML keeps
/// its type; anything else is the string that was typed, which is what makes
/// `--config units=mm` work without quoting.
fn config_table(config: &[(String, String)]) -> Result<toml_edit::InlineTable, String> {
    let mut table = toml_edit::InlineTable::new();
    for (key, raw) in config {
        let value = raw
            .parse::<Value>()
            .unwrap_or_else(|_| Value::from(raw.as_str()));
        table.insert(key, value);
    }
    Ok(table)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str) -> DocumentMut {
        text.parse::<DocumentMut>().unwrap()
    }

    /// The point of `toml_edit`: an author's comments and key order are
    /// theirs, and an edit is one line in the middle of them.
    #[test]
    fn an_edit_leaves_the_rest_of_the_file_alone() {
        let mut d = doc("# the app\n\
             [app]\n\
             name = \"demo\"   # trailing\n\
             \n\
             [dependencies]\n\
             # keep this one\n\
             lumen-fs = { bundled = true }\n");
        declare(&mut d, "shape-tools", "1.2", false, &[]).unwrap();
        let out = d.to_string();
        assert!(out.starts_with("# the app\n[app]\n"), "{out}");
        assert!(out.contains("name = \"demo\"   # trailing"), "{out}");
        assert!(out.contains("# keep this one"), "{out}");
        assert!(out.contains("shape-tools = \"1.2\""), "{out}");
    }

    #[test]
    fn a_config_table_rides_along_and_keeps_its_types() {
        let mut d = doc("");
        declare(
            &mut d,
            "shape",
            "1",
            false,
            &[
                ("units".to_string(), "mm".to_string()),
                ("cap".to_string(), "7".to_string()),
                ("loud".to_string(), "true".to_string()),
            ],
        )
        .unwrap();
        let out = d.to_string();
        assert!(out.contains("version = \"1\""), "{out}");
        assert!(out.contains("units = \"mm\""), "{out}");
        assert!(out.contains("cap = 7"), "{out}");
        assert!(out.contains("loud = true"), "{out}");
    }

    #[test]
    fn a_plugin_lands_in_its_own_array() {
        let mut d = doc("[app]\nname = \"demo\"\n");
        declare(&mut d, "markdown", "1.2", true, &[]).unwrap();
        let out = d.to_string();
        assert!(out.contains("[[plugins]]"), "{out}");
        assert!(out.contains("name = \"markdown\""), "{out}");
        assert!(out.contains("version = \"1.2\""), "{out}");
    }

    /// Adding a name twice re-pins it rather than declaring it twice.
    #[test]
    fn adding_again_moves_the_requirement() {
        let mut d = doc("[dependencies]\nshape = \"1\"\n");
        declare(&mut d, "shape", "2", false, &[]).unwrap();
        let out = d.to_string();
        assert!(out.contains("shape = \"2\""), "{out}");
        assert_eq!(out.matches("shape").count(), 1, "{out}");

        let mut d = doc("[[plugins]]\nname = \"md\"\nversion = \"1\"\n");
        declare(&mut d, "md", "2", true, &[]).unwrap();
        let out = d.to_string();
        assert!(out.contains("version = \"2\""), "{out}");
        assert_eq!(out.matches("[[plugins]]").count(), 1, "{out}");
    }

    /// A `path` source is a local build; re-adding the name as a registry
    /// package replaces the source rather than declaring two.
    #[test]
    fn a_registry_version_replaces_a_path_source() {
        let mut d = doc("[[plugins]]\nname = \"md\"\npath = \"plugins/md\"\n");
        declare(&mut d, "md", "1.2", true, &[]).unwrap();
        let out = d.to_string();
        assert!(!out.contains("path"), "{out}");
        assert!(out.contains("version = \"1.2\""), "{out}");
    }

    #[test]
    fn one_name_cannot_be_both_kinds() {
        let mut d = doc("[dependencies]\nmd = \"1\"\n");
        let err = declare(&mut d, "md", "1", true, &[]).unwrap_err();
        assert!(err.contains("[dependencies]"), "{err}");

        let mut d = doc("[[plugins]]\nname = \"md\"\nversion = \"1\"\n");
        let err = declare(&mut d, "md", "1", false, &[]).unwrap_err();
        assert!(err.contains("[[plugins]]"), "{err}");
    }

    #[test]
    fn removing_takes_the_entry_and_the_emptied_header_with_it() {
        let mut d = doc("[app]\nname = \"demo\"\n\n[dependencies]\nshape = \"1\"\n");
        assert!(undeclare(&mut d, "shape"));
        let out = d.to_string();
        assert!(!out.contains("dependencies"), "{out}");
        assert!(out.contains("[app]"), "{out}");

        let mut d = doc("[[plugins]]\nname = \"md\"\nversion = \"1\"\n");
        assert!(undeclare(&mut d, "md"));
        assert!(!d.to_string().contains("plugins"), "{}", d.to_string());
    }

    #[test]
    fn removing_a_name_nothing_declares_says_so() {
        let mut d = doc("[dependencies]\nshape = \"1\"\n");
        assert!(!undeclare(&mut d, "ghost"));
        assert!(d.to_string().contains("shape"));
    }

    /// One entry of two leaves the other, and the table.
    #[test]
    fn removing_one_of_two_keeps_the_table() {
        let mut d = doc("[dependencies]\nshape = \"1\"\nfs = { bundled = true }\n");
        assert!(undeclare(&mut d, "shape"));
        let out = d.to_string();
        assert!(out.contains("[dependencies]"), "{out}");
        assert!(out.contains("fs = { bundled = true }"), "{out}");
    }

    /// An add-then-remove round trip is the identity on everything the author
    /// wrote, which is the whole reason the edit is format-preserving.
    #[test]
    fn add_then_remove_leaves_the_file_as_it_was() {
        let before = "# demo app\n\
                      [app]\n\
                      name = \"demo\"\n\
                      \n\
                      [dependencies]\n\
                      # the filesystem module\n\
                      lumen-fs = { bundled = true }\n";
        let mut d = doc(before);
        declare(&mut d, "shape-tools", "1.2", false, &[]).unwrap();
        assert!(undeclare(&mut d, "shape-tools"));
        assert_eq!(d.to_string(), before);
    }
}
