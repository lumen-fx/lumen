//! What an `lmn!` block does inside a real candela compile.
//!
//! The unit tests beside the module cover what a block reads as. These drive
//! the expander through the host itself, which is the only place the
//! expansion, the host-function signatures, and candela's diagnostics meet.

use lumen_script::{ScriptError, ScriptHost};
use lumen_script_candela::{CandelaHost, compile_bytecode, lmn};

/// A script whose block spans five lines, with an unparseable statement on
/// line 11. candela parses an expansion as its own expression and pins every
/// span it produces to the invocation, so what the reader wrote is what the
/// diagnostic names.
const SPANNING: &str = r#"import "lumen.cdl";

fn Home(name) {
    return lmn!(
        <column>
            <label text="home for $name"/>
        </column>
    );
}

fn broken() { let x = ; }

fn main() {}
"#;

#[test]
fn a_multi_line_block_leaves_later_line_numbers_alone() {
    let host = CandelaHost::new();
    let error = host
        .compile_check(SPANNING, "spanning.cdl")
        .expect_err("line 11 does not parse");
    let ScriptError::Compile { line, .. } = error else {
        panic!("expected a compile diagnostic, got {error:?}");
    };
    assert_eq!(
        line, 11,
        "the diagnostic points at the line the reader wrote"
    );
}

#[test]
fn a_block_compiles_through_the_host() {
    let host = CandelaHost::new();
    host.compile_check(
        "import \"lumen.cdl\";\n\
         fn Home(name) { return lmn!(<label text=\"home for $name\"/>); }\n\
         fn App() { return lmn!(<column><Home name=\"bob\"/></column>); }\n\
         fn on_ready() { lumen::mount(App()); }\n\
         fn main() {}\n",
        "app.cdl",
    )
    .expect("a component script compiles");
}

#[test]
fn a_prop_naming_no_parameter_fails_the_compile() {
    let host = CandelaHost::new();
    let error = host
        .compile_check(
            "import \"lumen.cdl\";\n\
             fn Home(name) { return lmn!(<label text=\"$name\"/>); }\n\
             fn App() { return lmn!(<column><Home title=\"bob\"/></column>); }\n\
             fn main() {}\n",
            "app.cdl",
        )
        .expect_err("`title` is not a parameter of Home");
    let ScriptError::Compile { message, line, .. } = error else {
        panic!("expected a compile diagnostic, got {error:?}");
    };
    assert!(message.contains("<Home>"), "{message}");
    assert!(message.contains("title"), "{message}");
    assert_eq!(line, 3);
}

#[test]
fn a_block_naming_no_component_fails_the_compile() {
    let host = CandelaHost::new();
    let error = host
        .compile_check(
            "import \"lumen.cdl\";\n\
             fn App() { return lmn!(<column><Missing/></column>); }\n\
             fn main() {}\n",
            "app.cdl",
        )
        .expect_err("no function named Missing");
    let ScriptError::Compile { message, .. } = error else {
        panic!("expected a compile diagnostic, got {error:?}");
    };
    assert!(message.contains("<Missing>"), "{message}");
}

/// The bytecode path builds its own expander, so a block compiles to an image
/// the same way it compiles to a program.
#[test]
fn a_block_compiles_to_bytecode() {
    let bytes = compile_bytecode(
        "import \"lumen.cdl\";\n\
         fn Home(name) { return lmn!(<label text=\"home for $name\"/>); }\n\
         fn main() {}\n",
        "app.cdl",
    )
    .expect("the program compiles");
    assert!(!bytes.is_empty());
}

/// The expansion the compiler sees names the same key the extractor writes
/// into the artifact. This is what makes a shipped app find its fragment.
#[test]
fn the_expansion_names_the_key_the_extractor_writes() {
    let body = "<label text=\"home for $name\"/>";
    let expansion = lmn::expand(body, &lmn::FnIndex::default()).expect("expands");
    assert!(
        expansion.contains(&format!("\"{}\"", lmn::key_of(body))),
        "{expansion}"
    );
}
