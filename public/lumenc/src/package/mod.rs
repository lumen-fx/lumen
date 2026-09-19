//! Assembling a shippable app, and the registry client + release channel
//! that back it.

/// `lumenc package` - assemble a shippable app folder from the launcher stub,
/// the app's compiled artifact, the shared runtime library, and the app's own
/// files. Gated with the compile path it uses (`runtime-parse` + `dev-run`)
/// and with `package`, which carries the release-channel fetch `--target`
/// needs.
#[cfg(all(feature = "runtime-parse", feature = "dev-run", feature = "package"))]
pub mod cli;
/// `lpm`, the registry client. A `version` source in `[dependencies]` or
/// `[[plugins]]` names a registry package, and this is what asks `lpm` to
/// resolve, download, and lock it. Gated with the shape that compiles an app
/// from source and can fetch: a compiler that only loads a prebuilt artifact
/// resolves nothing.
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub mod lpm;
/// Which published release this toolchain draws its files from. Every download
/// location and cache directory is keyed by the answer.
pub mod release;
/// The daily "a newer release exists" notice an installed toolchain prints.
pub mod update_check;
