//! One storage area: the values, and the file they are kept in when they
//! outlast the process.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lumen_module::lumen_core::warn_line;

/// Where a store keeps its values between runs.
enum Backing {
    /// Nowhere: the values last as long as the process.
    Memory,
    /// A file, named by the closure the first time it is needed and read
    /// then.
    File {
        locate: Option<Box<dyn FnOnce() -> PathBuf + Send>>,
        path: PathBuf,
    },
}

/// Text values under text keys, read in key order.
pub struct Store {
    values: BTreeMap<String, String>,
    backing: Backing,
}

impl Store {
    /// A store that lasts as long as the process.
    #[must_use]
    pub fn memory() -> Self {
        Self {
            values: BTreeMap::new(),
            backing: Backing::Memory,
        }
    }

    /// A store kept in the file `locate` names, read on first use and
    /// written through on every change.
    #[must_use]
    pub fn persistent(locate: impl FnOnce() -> PathBuf + Send + 'static) -> Self {
        Self {
            values: BTreeMap::new(),
            backing: Backing::File {
                locate: Some(Box::new(locate)),
                path: PathBuf::new(),
            },
        }
    }

    /// The value under `key`.
    pub fn get(&mut self, key: &str) -> Option<String> {
        self.load();
        self.values.get(key).cloned()
    }

    /// Store `value` under `key`. False when the file could not be written,
    /// in which case nothing changed.
    pub fn set(&mut self, key: String, value: String) -> bool {
        self.load();
        let previous = self.values.insert(key.clone(), value);
        if self.save() {
            return true;
        }
        match previous {
            Some(previous) => self.values.insert(key, previous),
            None => self.values.remove(&key),
        };
        false
    }

    /// Remove `key`.
    pub fn remove(&mut self, key: &str) {
        self.load();
        if self.values.remove(key).is_some() {
            self.save();
        }
    }

    /// Every key, in order.
    pub fn keys(&mut self) -> Vec<String> {
        self.load();
        self.values.keys().cloned().collect()
    }

    /// Remove everything.
    pub fn clear(&mut self) {
        self.load();
        if !self.values.is_empty() {
            self.values.clear();
            self.save();
        }
    }

    /// Read the file the first time the store is used.
    fn load(&mut self) {
        let Backing::File { locate, path } = &mut self.backing else {
            return;
        };
        let Some(locate) = locate.take() else {
            return;
        };
        *path = locate();
        self.values = read(path);
    }

    /// Write the values to the file, when there is one.
    fn save(&self) -> bool {
        match &self.backing {
            Backing::Memory => true,
            Backing::File { path, .. } => match write(path, &self.values) {
                Ok(()) => true,
                Err(e) => {
                    warn_line!("lumen-storage: write {}: {e}", path.display());
                    false
                }
            },
        }
    }
}

/// The values in `path`. A file that is not there is an empty store; one that
/// does not read is kept aside under another name, so the next write does not
/// replace what somebody may still want back.
fn read(path: &Path) -> BTreeMap<String, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return BTreeMap::new(),
        Err(e) => {
            warn_line!("lumen-storage: read {}: {e}", path.display());
            return BTreeMap::new();
        }
    };
    match serde_json::from_str(&text) {
        Ok(values) => values,
        Err(e) => {
            let aside = path.with_extension("json.unreadable");
            warn_line!(
                "lumen-storage: {} is not a JSON object of strings ({e}); moved to {} and \
                 starting empty",
                path.display(),
                aside.display()
            );
            let _ = std::fs::rename(path, aside);
            BTreeMap::new()
        }
    }
}

/// Write `values` to `path` through a file beside it, so a crash mid-write
/// leaves the old file whole.
fn write(path: &Path, values: &BTreeMap<String, String>) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(values).map_err(std::io::Error::other)?;
    let partial = path.with_extension("json.partial");
    std::fs::write(&partial, text)?;
    std::fs::rename(&partial, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lumen-storage-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_persistent_store_reads_back_what_an_earlier_one_wrote() {
        let dir = scratch("persist");
        let file = dir.join("storage.json");
        let mut first = Store::persistent({
            let file = file.clone();
            move || file
        });
        assert_eq!(first.get("a"), None);
        assert!(first.set("b".into(), "2".into()));
        assert!(first.set("a".into(), "1".into()));
        first.remove("b");
        assert!(first.set("c".into(), "3".into()));

        let mut second = Store::persistent({
            let file = file.clone();
            move || file
        });
        assert_eq!(second.keys(), ["a", "c"]);
        assert_eq!(second.get("a").as_deref(), Some("1"));
        second.clear();
        let mut third = Store::persistent(move || file);
        assert!(third.keys().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreadable_file_is_kept_aside() {
        let dir = scratch("unreadable");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("storage.json");
        std::fs::write(&file, "not json").unwrap();
        let mut store = Store::persistent({
            let file = file.clone();
            move || file
        });
        assert!(store.keys().is_empty());
        assert_eq!(
            std::fs::read_to_string(dir.join("storage.json.unreadable")).unwrap(),
            "not json"
        );
        assert!(store.set("k".into(), "v".into()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_write_that_fails_changes_nothing() {
        let dir = scratch("readonly");
        std::fs::create_dir_all(&dir).unwrap();
        // A directory where the file should be: every write fails.
        let file = dir.join("storage.json");
        std::fs::create_dir_all(&file).unwrap();
        let mut store = Store::persistent(move || file);
        assert!(!store.set("k".into(), "v".into()));
        assert_eq!(store.get("k"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_memory_store_writes_nothing() {
        let mut store = Store::memory();
        assert!(store.set("k".into(), "v".into()));
        assert_eq!(store.keys(), ["k"]);
        store.clear();
        assert!(store.keys().is_empty());
    }
}
