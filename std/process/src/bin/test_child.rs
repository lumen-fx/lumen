//! The program the process module's tests run as a child.
//!
//! ```text
//! lumen-process-test-child [code] [--lines N] [--cwd] [--env NAME]
//!                           [--sleep MS] [more arguments...]
//! ```
//!
//! Every argument is echoed to stdout on a line of its own, in the order it
//! arrived, which is how a test sees that the argument list crossed intact.
//! `--lines N` then writes `line-1` through `line-N`, for a test that wants
//! more output than it wants to spell out. `--cwd` writes `cwd=` and the
//! directory it runs in, and `--env NAME` writes `env=` and that variable's
//! value (empty when unset). `--sleep MS` then waits that many milliseconds,
//! for a test that needs a child still running. One line goes to stderr, and
//! the program exits with the code named by its first argument, or zero when
//! there is no number there.
//!
//! It ships as a binary of the module crate so the integration tests reach it
//! through `CARGO_BIN_EXE_lumen-process-test-child`; nothing in the module
//! itself uses it.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    for arg in &args {
        println!("{arg}");
    }

    if let Some(i) = args.iter().position(|a| a == "--lines") {
        let count: usize = args
            .get(i + 1)
            .and_then(|n| n.parse().ok())
            .unwrap_or_default();
        for n in 1..=count {
            println!("line-{n}");
        }
    }

    if args.iter().any(|a| a == "--cwd") {
        let cwd = std::env::current_dir().unwrap_or_default();
        println!("cwd={}", cwd.display());
    }

    if let Some(name) = value_after(&args, "--env") {
        println!("env={}", std::env::var(name).unwrap_or_default());
    }

    eprintln!("child stderr");

    if let Some(ms) = value_after(&args, "--sleep").and_then(|n| n.parse().ok()) {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }

    let code: i32 = args
        .first()
        .and_then(|a| a.parse().ok())
        .unwrap_or_default();
    std::process::exit(code);
}

/// The argument after `flag`, when `flag` is there.
fn value_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let i = args.iter().position(|a| a == flag)?;
    args.get(i + 1).map(String::as_str)
}
