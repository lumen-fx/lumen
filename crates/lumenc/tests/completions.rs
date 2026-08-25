//! Keeps the shipped shell completions in step with the CLI they complete.
//!
//! The scripts are hand-written (lumenc parses its own arguments), so nothing
//! stops them drifting from the binary except a test that reads both. Every
//! assertion here runs the real `lumenc` and compares its help output against
//! what the scripts offer:
//!
//! - the subcommand sets match, in both directions;
//! - every flag a script offers for a subcommand appears in that
//!   subcommand's own `--help`;
//! - the templates offered for `lumenc new` are the scaffold gallery, in
//!   gallery order;
//! - `lumenc completions <shell>` prints the shipped file byte for byte.

use std::collections::BTreeSet;
use std::process::Command;

use lumenc::scaffold::TEMPLATES;

const BASH: &str = include_str!("../completions/lumenc.bash");
const ZSH: &str = include_str!("../completions/_lumenc");
const FISH: &str = include_str!("../completions/lumenc.fish");

/// Run `lumenc <args>` and return stdout. Help goes to stdout on success.
fn lumenc(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_lumenc"))
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("running lumenc {args:?}: {e}"));
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        panic!(
            "lumenc {args:?} printed nothing on stdout; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    stdout
}

/// Subcommands named in the top-level usage block: the lines that start with
/// exactly four spaces and `lumenc `. Flag-only lines (`lumenc --help`) are
/// not subcommands.
fn commands_from_help() -> BTreeSet<String> {
    lumenc(&["--help"])
        .lines()
        .filter_map(|line| line.strip_prefix("    lumenc "))
        .filter_map(|rest| rest.split_whitespace().next())
        .filter(|word| !word.starts_with('-'))
        .map(str::to_string)
        .collect()
}

/// The bash script keeps its command list in one assignment.
fn commands_from_bash() -> BTreeSet<String> {
    let line = BASH
        .lines()
        .find(|l| l.starts_with("_lumenc_commands="))
        .expect("bash completion has no _lumenc_commands= line");
    line.trim_start_matches("_lumenc_commands=")
        .trim_matches('"')
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// The zsh script keeps its commands as `'name:description'` entries inside
/// one `commands=( ... )` array.
fn commands_from_zsh() -> BTreeSet<String> {
    let start = ZSH
        .find("commands=(")
        .expect("zsh completion has no commands=(");
    let rest = &ZSH[start..];
    let end = rest
        .find("\n    )")
        .expect("unterminated commands=( in the zsh completion");
    rest[..end]
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('\''))
        .filter_map(|l| l.trim_matches('\'').split(':').next().map(str::to_string))
        .collect()
}

/// The fish script offers each command with `-n __fish_use_subcommand -a NAME`.
fn commands_from_fish() -> BTreeSet<String> {
    FISH.lines()
        .filter(|l| l.contains("-n __fish_use_subcommand"))
        .filter_map(|l| {
            let rest = l.split(" -a ").nth(1)?;
            let word = rest.split_whitespace().next()?;
            (!word.starts_with('-')).then(|| word.to_string())
        })
        .collect()
}

#[test]
fn every_script_completes_exactly_the_binary_s_subcommands() {
    let expected = commands_from_help();
    assert!(
        expected.contains("run") && expected.contains("completions"),
        "the help parser found no plausible command set: {expected:?}"
    );
    for (shell, found) in [
        ("bash", commands_from_bash()),
        ("zsh", commands_from_zsh()),
        ("fish", commands_from_fish()),
    ] {
        let missing: Vec<_> = expected.difference(&found).collect();
        let extra: Vec<_> = found.difference(&expected).collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "the {shell} completion is out of step with `lumenc --help`: \
             missing {missing:?}, not a lumenc subcommand {extra:?}"
        );
    }
}

/// Flags offered for a subcommand, per script. `--help` is universal and
/// never listed in a usage block, so it is excluded everywhere.
fn flags_from_bash(command: &str) -> BTreeSet<String> {
    let needle = format!("        {command}) flags=\"");
    let line = BASH
        .lines()
        .find(|l| l.starts_with(&needle))
        .unwrap_or_else(|| panic!("bash completion has no flag list for `{command}`"));
    line.trim_start()
        .trim_start_matches(&format!("{command}) flags=\""))
        .split('"')
        .next()
        .unwrap_or("")
        .split_whitespace()
        .filter(|f| f.starts_with("--"))
        .map(str::to_string)
        .collect()
}

/// Long flags a script line offers, from either a zsh `'--flag[...]'` spec or
/// a fish `-l flag` argument.
fn zsh_flags(block: &str) -> BTreeSet<String> {
    block
        .lines()
        .map(str::trim)
        .filter_map(|l| l.strip_prefix("'--"))
        .filter_map(|l| l.split(['[', ':']).next())
        .map(|f| format!("--{f}"))
        .collect()
}

/// The `case` arm for one subcommand in the zsh script. Line endings are
/// normalized first so the exact-boundary search cannot miss on a checkout
/// that converted the script to CRLF.
fn zsh_block(command: &str) -> String {
    let zsh = ZSH.replace("\r\n", "\n");
    let start = zsh
        .find(&format!("\n                {command})\n"))
        .unwrap_or_else(|| panic!("zsh completion has no arm for `{command}`"));
    let rest = &zsh[start + 1..];
    let end = rest.find("\n                    ;;").unwrap_or(rest.len());
    rest[..end].to_string()
}

fn flags_from_fish(command: &str) -> BTreeSet<String> {
    FISH.lines()
        .filter(|l| l.starts_with("complete -c lumenc"))
        .filter(|l| {
            // Either a per-command condition or one of the grouped
            // `$lumenc_*` sets, which are expanded from the `set -l` lines.
            let condition = expand_fish_sets(&fish_condition(l).unwrap_or_default());
            condition.contains("__fish_seen_subcommand_from")
                && condition
                    .split_whitespace()
                    .any(|w| w.trim_matches(|c| c == '"' || c == '\'' || c == ';') == command)
        })
        .filter_map(|l| l.split(" -l ").nth(1))
        .filter_map(|rest| rest.split_whitespace().next())
        .map(|f| format!("--{f}"))
        .collect()
}

/// The condition a `complete` line carries after `-n`, quoted with either
/// quote character or left bare.
fn fish_condition(line: &str) -> Option<String> {
    let rest = line.split(" -n ").nth(1)?;
    let mut chars = rest.chars();
    match chars.next()? {
        q @ ('"' | '\'') => rest[1..].split(q).next().map(str::to_string),
        _ => rest.split_whitespace().next().map(str::to_string),
    }
}

/// Replace `$lumenc_mcp` and friends with the command names their `set -l`
/// line defines.
fn expand_fish_sets(condition: &str) -> String {
    let mut out = condition.to_string();
    for line in FISH.lines().filter(|l| l.starts_with("set -l lumenc_")) {
        let mut words = line["set -l ".len()..].split_whitespace();
        let name = words.next().unwrap_or_default();
        let values: Vec<&str> = words.collect();
        out = out.replace(&format!("${name}"), &values.join(" "));
    }
    out
}

#[test]
fn every_completed_flag_exists_in_that_subcommand_s_help() {
    // `check` takes no flags of its own, so it has no list to compare.
    let commands = [
        "run",
        "build",
        "new",
        "fmt",
        "snapshot",
        "find",
        "element-at",
        "click",
        "type",
        "key",
        "scroll",
        "lint",
        "diff",
        "screenshot",
        "web",
        "bundle",
        "package",
        "i18n",
    ];
    for command in commands {
        let help = lumenc(&[command, "--help"]);
        let mut offered = flags_from_bash(command);
        offered.extend(zsh_flags(&zsh_block(command)));
        offered.extend(flags_from_fish(command));
        offered.remove("--help");
        assert!(
            !offered.is_empty(),
            "no flags were read out of the completions for `{command}`; the \
             test's parser is looking in the wrong place"
        );
        for flag in offered {
            assert!(
                help.contains(&flag),
                "the completions offer `{flag}` for `lumenc {command}`, but \
                 `lumenc {command} --help` does not mention it"
            );
        }
    }
}

/// The template names one script offers as the second argument to
/// `lumenc new`, in the order the script lists them.
fn templates_from_script(shell: &str) -> Vec<String> {
    let list = match shell {
        // The bash script has two `new)` arms: a one-line one listing the
        // flags, and the argument arm, which is the one that opens a block.
        // Read only inside that arm, so a list moved out of it fails here
        // rather than matching the next subcommand's word list.
        "bash" => BASH
            .split_once("        new)\n")
            .and_then(|(_, rest)| rest.split_once("\n            ;;"))
            .and_then(|(arm, _)| arm.split_once("compgen -W \""))
            .and_then(|(_, rest)| rest.split('"').next()),
        "zsh" => ZSH
            .split_once("'2:template:(")
            .and_then(|(_, rest)| rest.split(')').next()),
        "fish" => FISH
            .lines()
            .find(|line| line.contains("__fish_seen_subcommand_from new") && line.contains(" -a '"))
            .and_then(|line| line.split_once(" -a '"))
            .and_then(|(_, rest)| rest.split('\'').next()),
        other => panic!("no reader for the {other} completion"),
    };
    list.unwrap_or_else(|| panic!("the {shell} completion offers no templates for `new`"))
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

#[test]
fn every_script_offers_the_scaffold_gallery_in_gallery_order() {
    let expected: Vec<String> = TEMPLATES.iter().map(|t| t.name.to_string()).collect();
    assert!(
        expected.len() > 1,
        "the scaffold gallery is implausibly small: {expected:?}"
    );
    for shell in ["bash", "zsh", "fish"] {
        assert_eq!(
            templates_from_script(shell),
            expected,
            "the {shell} completion offers other `lumenc new` templates than the gallery"
        );
    }
}

/// The modes `lumenc web --help` says `--render` takes, read off the usage
/// block's `--render static|csr|ssr`.
fn render_modes_from_help() -> BTreeSet<String> {
    lumenc(&["web", "--help"])
        .split_once("--render ")
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("`lumenc web --help` names no modes for --render"))
        .trim_end_matches(']')
        .split('|')
        .map(str::to_string)
        .collect()
}

/// The values one script offers for `lumenc web --render`, taken from the
/// list each shell spells its own way.
fn render_modes_from_script(shell: &str) -> BTreeSet<String> {
    let list = match shell {
        "bash" => BASH
            .split_once("\"web --render\")")
            .and_then(|(_, rest)| rest.split_once("-W \""))
            .and_then(|(_, rest)| rest.split('"').next()),
        "zsh" => ZSH
            .split_once("'--render[")
            .and_then(|(_, rest)| rest.split_once(":mode:("))
            .and_then(|(_, rest)| rest.split(')').next()),
        "fish" => FISH
            .lines()
            .find(|line| line.contains(" -l render "))
            .and_then(|line| line.split_once(" -a '"))
            .and_then(|(_, rest)| rest.split('\'').next()),
        other => panic!("no reader for the {other} completion"),
    };
    list.unwrap_or_else(|| panic!("the {shell} completion offers no modes for --render"))
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

#[test]
fn every_script_offers_the_render_modes_the_command_takes() {
    let expected = render_modes_from_help();
    assert!(
        expected.len() > 1,
        "the help parser found no plausible mode set: {expected:?}"
    );
    for shell in ["bash", "zsh", "fish"] {
        assert_eq!(
            render_modes_from_script(shell),
            expected,
            "the {shell} completion offers other `lumenc web --render` modes than the command takes"
        );
    }
}

#[test]
fn the_subcommand_prints_the_shipped_scripts_verbatim() {
    for (shell, shipped) in [("bash", BASH), ("zsh", ZSH), ("fish", FISH)] {
        assert_eq!(
            lumenc(&["completions", shell]),
            shipped,
            "`lumenc completions {shell}` does not match the shipped file"
        );
    }
}

#[test]
fn an_unknown_shell_is_a_usage_error() {
    let out = Command::new(env!("CARGO_BIN_EXE_lumenc"))
        .args(["completions", "tcsh"])
        .output()
        .expect("running lumenc");
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
}
