//! The generated half of the prelude is up to date.
//!
//! `declarations.cdl` is written from the shared builtin table plus the
//! declarations that sit beside the host's own registrations, so a builtin
//! that changes shape changes the prelude with it. The file is checked in
//! because the compiler reads it through `include_str!` and a build script
//! that wrote it would put the whole table in front of every consumer.

use lumen_script_candela::prelude;

#[test]
fn the_generated_declarations_are_current() {
    let generated = prelude::generate_declarations();
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/prelude/declarations.cdl");

    if std::env::var_os("UPDATE_PRELUDE").is_some() {
        std::fs::write(path, &generated).expect("write the refreshed declarations");
        return;
    }

    let current = std::fs::read_to_string(path).expect("read the checked-in declarations");
    assert_eq!(
        current, generated,
        "prelude/declarations.cdl is stale; refresh it with\n    \
         UPDATE_PRELUDE=1 cargo test -p lumen-script-candela --test prelude_generated"
    );
}
