//! Linking a runtime-module link kit into one executable, writing the kit a
//! release ships, and dlopening it for the link-not-embed launcher.

/// `lumenc package --static` - link one executable out of the per-target link
/// kit a release publishes, with the app's declared runtime modules compiled
/// in. Gated with `package::cli`, whose folder assembly it is one arm of.
#[cfg(all(feature = "runtime-parse", feature = "dev-run", feature = "package"))]
pub mod kit;
/// `lumenc link-kit emit` - write the per-target link kit a release ships,
/// out of a recorded link and the files that link read. A release step rather
/// than a command anyone runs by hand, so it is absent from `lumenc --help`.
/// Gated with `package::cli`, whose target table names the release assets.
#[cfg(all(feature = "runtime-parse", feature = "dev-run", feature = "package"))]
pub mod kit_cli;
/// dlopen loader for the link-not-embed launcher: discover + open the shared
/// liblumen, verify its ABI, and drive a prebuilt LMNA app across the C-ABI.
/// The crate's only `unsafe`: dynamic symbol resolution and FFI calls, audited
/// against the C-ABI contract in the root `lumen` crate.
#[cfg(feature = "dlopen-run")]
#[allow(unsafe_code)]
pub mod loader;
