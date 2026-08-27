//! The load handshake: what the loader accepts, what it refuses, and what it
//! says about the refusal.
//!
//! The arms a well-formed plugin can reach are driven through the fixture.
//! The ones no plugin produces (a foreign ABI version, a wire-version skew,
//! a truncated descriptor, a missing name) are hand-built descriptors, in
//! this crate's own unit tests.

mod common;

use std::sync::Arc;

use common::{Recorder, app_dir, env, fixture_module, load_fixture, module};
use lumen_plugin::{FailureReason, HostHooks, PluginSet, testing};

#[test]
fn a_well_formed_plugin_loads() {
    let (set, failures, _hooks) = load_fixture("load-ok", "");
    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(set.len(), 1);
    assert_eq!(set.names().collect::<Vec<_>>(), ["lumen-plugin-fixture"]);
    assert!(!set.is_empty());
}

#[test]
fn a_missing_file_names_the_path_it_probed() {
    let dir = app_dir("load-missing");
    let ghost = dir.join("libghost.so");
    let (set, failures) = PluginSet::load(
        &[module("ghost", &ghost, "")],
        &env(&dir),
        Arc::new(Recorder::default()) as Arc<dyn HostHooks>,
    );
    assert!(set.is_empty());
    let failure = &failures[0];
    assert!(matches!(failure.reason, FailureReason::Open(_)));
    assert_eq!(failure.path, ghost);
    let message = failure.to_string();
    assert!(message.contains("plugin 'ghost'"), "{message}");
    assert!(message.contains("libghost.so"), "{message}");
}

#[cfg(target_os = "linux")]
#[test]
fn a_library_that_is_not_a_plugin_says_so() {
    // Any real shared library that is not a Lumen plugin serves; libc is
    // what the test binary itself is already linked against.
    let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
    let libc = maps
        .lines()
        .filter_map(|l| l.split_whitespace().last())
        .find(|p| {
            p.rsplit('/')
                .next()
                .is_some_and(|f| f.starts_with("libc.so"))
        })
        .expect("the test binary maps libc");
    let dir = app_dir("load-not-a-plugin");
    let (_set, failures) = PluginSet::load(
        &[module("libc", std::path::Path::new(libc), "")],
        &env(&dir),
        Arc::new(Recorder::default()) as Arc<dyn HostHooks>,
    );
    assert!(matches!(failures[0].reason, FailureReason::MissingEntry));
    assert!(
        failures[0].to_string().contains("lumen_plugin_v1"),
        "{}",
        failures[0]
    );
}

#[test]
fn a_compiler_plugin_is_named_for_what_it_is() {
    let dir = app_dir("load-compiler-plugin");
    let compiler_fixture = lumenc_plugin::testing::fixture_cdylib();
    let (_set, failures) = PluginSet::load(
        &[module("lumenc-plugin-fixture", &compiler_fixture, "")],
        &env(&dir),
        Arc::new(Recorder::default()) as Arc<dyn HostHooks>,
    );
    assert!(matches!(failures[0].reason, FailureReason::CompilerPlugin));
    let message = failures[0].to_string();
    assert!(message.contains("this is a compiler plugin"), "{message}");
    assert!(message.contains("[[plugins]]"), "{message}");
}

#[test]
fn a_library_reporting_another_name_is_refused() {
    let dir = app_dir("load-name");
    let (_set, failures) = PluginSet::load(
        &[module(
            "something-else",
            &testing::fixture_copy("load-name"),
            "",
        )],
        &env(&dir),
        Arc::new(Recorder::default()) as Arc<dyn HostHooks>,
    );
    assert!(matches!(
        &failures[0].reason,
        FailureReason::NameMismatch { reported } if reported == "lumen-plugin-fixture"
    ));
}

#[test]
fn one_library_declared_twice_is_refused_the_second_time() {
    let dir = app_dir("load-twice");
    let path = testing::fixture_copy("load-twice");
    let (set, failures) = PluginSet::load(
        &[
            module("lumen-plugin-fixture", &path, ""),
            module("lumen-plugin-fixture", &path, "fn_count = 1"),
        ],
        &env(&dir),
        Arc::new(Recorder::default()) as Arc<dyn HostHooks>,
    );
    assert_eq!(set.len(), 1);
    assert!(matches!(
        &failures[0].reason,
        FailureReason::AlreadyLoaded(prior) if prior == "lumen-plugin-fixture"
    ));
}

#[test]
fn a_module_that_fails_in_init_is_collected_and_the_others_still_load() {
    let dir = app_dir("load-init-fail");
    let hooks = Arc::new(Recorder::default()) as Arc<dyn HostHooks>;
    let (set, failures) = PluginSet::load(
        &[
            fixture_module("load-init-fail-a", "fail_in_init = true"),
            fixture_module("load-init-fail-b", ""),
        ],
        &env(&dir),
        hooks,
    );
    assert_eq!(set.len(), 1, "the healthy module loaded");
    assert_eq!(failures.len(), 1);
    assert!(
        matches!(&failures[0].reason, FailureReason::Init(m) if m == "fixture failure in init")
    );
}

#[test]
fn an_init_that_panics_is_a_failure_not_an_abort() {
    let (set, failures, _hooks) = load_fixture("load-init-panic", "panic_in_init = true");
    assert!(set.is_empty());
    assert!(matches!(
        &failures[0].reason,
        FailureReason::InitPanicked(m) if m == "fixture panic in init"
    ));
}

#[test]
fn a_manifest_claiming_what_it_may_not_is_refused() {
    for (tag, config, want) in [
        (
            "load-builtin-ns",
            "declare_builtin_ns = true",
            "builtin namespace",
        ),
        ("load-empty-hosts", "empty_hosts = true", "no language"),
        (
            "load-duplicate",
            "duplicate_name = true",
            "declares 'fixture_echo' twice",
        ),
    ] {
        let (set, failures, _hooks) = load_fixture(tag, config);
        assert!(set.is_empty(), "{tag}");
        assert!(
            matches!(&failures[0].reason, FailureReason::BadManifest(m) if m.contains(want)),
            "{tag}: {}",
            failures[0]
        );
    }
}
