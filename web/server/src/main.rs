//! `lumen-server`: serve a Lumen site that renders every page per request.

use std::process::ExitCode;

fn main() -> ExitCode {
    lumen_server::cli::main(std::env::args().skip(1).collect())
}
