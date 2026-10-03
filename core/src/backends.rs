//! The registry every backend kind shares.
//!
//! A backend kind (renderers, layout engines, window systems) is a trait in
//! [`crate::traits`] plus an entry type that names one implementation, ranks
//! it, and says how to build it. Each implementation registers its entry
//! into the app's [`Backends`] registry from its own crate while the app is
//! built, and a launch asks the registry for one by name or for every entry
//! in priority order. The core names no implementation, so a binary that
//! does not link a backend has no entry for it and pays nothing for it.

use crate::app::App;
use bevy_ecs::prelude::Resource;

/// What a registry entry tells the registry about the backend it names.
///
/// Entries are small `Copy` values (a name, a rank, a constructor function
/// pointer), so a launch takes them out of the registry and builds the
/// backend without holding the world.
pub trait BackendEntry: Copy + std::fmt::Debug + Send + Sync + 'static {
    /// The backend kind, for messages: `"render"`, `"layout"`, `"window"`.
    const KIND: &'static str;

    /// The name the backend is registered and selected under.
    fn name(&self) -> &'static str;

    /// Trial order when nothing names a backend: higher first.
    fn priority(&self) -> i32;
}

/// Main-world registry of the backends of one kind this binary carries.
#[derive(Resource, Clone, Debug)]
pub struct Backends<B: BackendEntry> {
    entries: Vec<B>,
}

impl<B: BackendEntry> Default for Backends<B> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

impl<B: BackendEntry> Backends<B> {
    /// Add `backend`, replacing one registered under the same name.
    pub fn register(&mut self, backend: B) {
        self.entries.retain(|b| b.name() != backend.name());
        self.entries.push(backend);
    }

    /// The backend registered as `name`.
    pub fn get(&self, name: &str) -> Option<&B> {
        self.entries.iter().find(|b| b.name() == name)
    }

    /// Every backend, highest priority first; equal priorities in name order,
    /// so the order is the same in every process.
    pub fn by_priority(&self) -> Vec<B> {
        let mut list = self.entries.clone();
        list.sort_by(|a, b| b.priority().cmp(&a.priority()).then(a.name().cmp(b.name())));
        list
    }

    /// The highest-priority backend, for a kind a launch uses exactly one of
    /// and no configuration names.
    pub fn preferred(&self) -> Option<B> {
        self.by_priority().into_iter().next()
    }

    /// The registered names, sorted, for a message naming what is available.
    pub fn names(&self) -> Vec<&'static str> {
        let mut names: Vec<&'static str> = self.entries.iter().map(|b| b.name()).collect();
        names.sort_unstable();
        names
    }

    /// The backends a launch should try, in order: the one `choice` names,
    /// or every backend by priority when `choice` is `None`. An error names
    /// what was asked for and what this binary carries.
    pub fn select(&self, choice: Option<&str>) -> Result<Vec<B>, String> {
        match choice {
            Some(name) => self.get(name).map(|b| vec![*b]).ok_or_else(|| {
                format!(
                    "the app asks for the '{name}' {} backend, and this build carries {}",
                    B::KIND,
                    describe(&self.names())
                )
            }),
            None if self.entries.is_empty() => Err(missing::<B>()),
            None => Ok(self.by_priority()),
        }
    }
}

/// The message for a build that carries no backend of kind `B`.
pub fn missing<B: BackendEntry>() -> String {
    format!("this build carries no {} backend", B::KIND)
}

/// `"none"`, `"only 'a'"`, or `"'a' and 'b'"`, for an error message.
fn describe(names: &[&str]) -> String {
    match names {
        [] => "none".to_string(),
        [one] => format!("only '{one}'"),
        many => {
            let quoted: Vec<String> = many.iter().map(|n| format!("'{n}'")).collect();
            let (last, rest) = quoted.split_last().expect("at least two");
            format!("{} and {last}", rest.join(", "))
        }
    }
}

/// Register `backend` into `app`'s registry of its kind, creating the
/// registry on first use. What a backend's capability install calls.
pub fn register_backend<B: BackendEntry>(app: &mut App, backend: B) {
    app.world
        .get_resource_or_insert_with(Backends::<B>::default)
        .register(backend);
}

/// The entries of kind `B` registered into `app`, empty when none are.
pub fn registered<B: BackendEntry>(app: &App) -> Backends<B> {
    app.world
        .get_resource::<Backends<B>>()
        .cloned()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug)]
    struct Entry {
        name: &'static str,
        priority: i32,
    }

    impl BackendEntry for Entry {
        const KIND: &'static str = "test";
        fn name(&self) -> &'static str {
            self.name
        }
        fn priority(&self) -> i32 {
            self.priority
        }
    }

    fn entry(name: &'static str, priority: i32) -> Entry {
        Entry { name, priority }
    }

    #[test]
    fn a_name_selects_one_backend_and_none_selects_all_by_priority() {
        let mut backends = Backends::default();
        backends.register(entry("slow", 0));
        backends.register(entry("fast", 10));
        backends.register(entry("also-slow", 0));

        let named = backends.select(Some("slow")).expect("registered");
        assert_eq!(named.len(), 1);
        assert_eq!(named[0].name, "slow");

        let order: Vec<&str> = backends
            .select(None)
            .expect("some")
            .iter()
            .map(|b| b.name)
            .collect();
        assert_eq!(order, ["fast", "also-slow", "slow"]);
        assert_eq!(backends.preferred().map(|b| b.name), Some("fast"));
    }

    #[test]
    fn asking_for_a_backend_the_build_lacks_says_what_it_has() {
        let mut backends = Backends::default();
        let none = backends.select(Some("cpu")).expect_err("empty");
        assert!(
            none.contains("'cpu' test backend") && none.contains("none"),
            "{none}"
        );
        assert_eq!(
            backends.select(None).expect_err("empty"),
            "this build carries no test backend"
        );
        assert!(backends.preferred().is_none());

        backends.register(entry("gpu", 10));
        let one = backends.select(Some("cpu")).expect_err("missing");
        assert!(one.contains("only 'gpu'"), "{one}");

        backends.register(entry("other", 0));
        backends.register(entry("third", 0));
        let many = backends.select(Some("cpu")).expect_err("missing");
        assert!(many.contains("'gpu', 'other' and 'third'"), "{many}");
    }

    #[test]
    fn registering_a_name_again_replaces_it() {
        let mut app = App::new();
        register_backend(&mut app, entry("gpu", 1));
        register_backend(&mut app, entry("gpu", 5));
        let backends = registered::<Entry>(&app);
        assert_eq!(backends.names(), ["gpu"]);
        assert_eq!(backends.get("gpu").map(|b| b.priority), Some(5));
    }
}
