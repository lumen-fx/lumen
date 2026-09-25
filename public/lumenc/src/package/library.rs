//! Which kind of library a declared dependency is, read off the file's export
//! table.
//!
//! The runtime tells the kinds apart by the entry each exports, and it finds
//! out by opening the file. A package cannot do that: the file may be another
//! platform's build, and opening one runs its initializers. Reading the table
//! answers the same question for an ELF, Mach-O, or PE file on any machine.

use std::path::Path;

use lumen_modules::PLUGIN_ENTRY_SYMBOL;
use object::{BinaryFormat, NameOrOrdinal, Object};

/// Whether the library at `path` is a portable plugin: it exports
/// `lumen_plugin_v1`, which is the entry the runtime loads one through on
/// every platform.
///
/// A file that cannot be read or parsed exports nothing, so it is not one.
pub fn is_portable_plugin(path: &Path) -> bool {
    std::fs::read(path).is_ok_and(|bytes| exports(&bytes, PLUGIN_ENTRY_SYMBOL))
}

/// Whether the library image `bytes` exports `symbol`. A Mach-O export
/// carries the C symbol with a leading underscore, which is how the same
/// source-level name is spelled there.
pub fn exports(bytes: &[u8], symbol: &str) -> bool {
    let Ok(file) = object::File::parse(bytes) else {
        return false;
    };
    let mangled = format!("_{symbol}");
    let wanted: &[u8] = if file.format() == BinaryFormat::MachO {
        mangled.as_bytes()
    } else {
        symbol.as_bytes()
    };
    let Ok(mut exported) = file.exports() else {
        return false;
    };
    exported.any(|export| export.is_ok_and(|export| export.name() == NameOrOrdinal::Name(wanted)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_that_is_no_library_exports_nothing() {
        assert!(!exports(b"module bytes", PLUGIN_ENTRY_SYMBOL));
        assert!(!exports(&[], PLUGIN_ENTRY_SYMBOL));
    }

    #[test]
    fn a_missing_file_is_not_a_portable_plugin() {
        assert!(!is_portable_plugin(Path::new("/no/such/library.dll")));
    }
}
