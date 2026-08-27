//! The constructor-panic path needs first-call construction, and the
//! fixture's singleton is per library, so this test lives in its own binary:
//! the environment variable it sets is process-wide.

mod common;

use common::load_fixture;
use lumen_plugin::FailureReason;

#[test]
fn a_panicking_constructor_fails_the_module_not_the_app() {
    unsafe { std::env::set_var("LUMEN_RT_FIXTURE_CTOR_PANIC", "1") };
    let (set, failures, _hooks) = load_fixture("ctor-panic", "");
    assert!(set.is_empty());
    assert!(matches!(
        &failures[0].reason,
        FailureReason::InitPanicked(m) if m == "fixture panic in constructor"
    ));
}
