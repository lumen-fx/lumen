//! A stand-in for `lpm`, for the tests that drive the registry seam without a
//! registry. Compiled by `rustc` from this file; `LPM_BIN` points `lumenc` at
//! the result.
//!
//! It answers the two things `lumenc` asks of `lpm`:
//!
//! - `--version`, with a number no [`MIN_VERSION`] check rejects, so nothing
//!   tries to download a replacement.
//! - a resolution, printed from the file `LPM_STUB_JSON` names.
//!
//! `LPM_STUB_ARGV` names a file the argument list is written to, one argument
//! per line, so a test can assert on the command line `lumenc` built.
//! `LPM_STUB_FAIL` makes it fail with that message on stderr and that exit
//! code, for the paths where the registry says no.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--version") {
        println!("lpm version 99.0.0 (stub)");
        return;
    }
    if let Ok(path) = std::env::var("LPM_STUB_ARGV") {
        std::fs::write(path, args.join("\n")).expect("write the argument list");
    }
    if let Ok(message) = std::env::var("LPM_STUB_FAIL") {
        eprintln!("{message}");
        let code = std::env::var("LPM_STUB_EXIT")
            .ok()
            .and_then(|c| c.parse::<i32>().ok())
            .unwrap_or(1);
        std::process::exit(code);
    }
    match std::env::var("LPM_STUB_JSON") {
        Ok(path) => {
            let json = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {path}: {e}"));
            print!("{json}");
        }
        Err(_) => {
            eprintln!("lpm stub: neither LPM_STUB_JSON nor LPM_STUB_FAIL is set");
            std::process::exit(1);
        }
    }
}
