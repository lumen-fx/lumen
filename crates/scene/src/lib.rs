//! The scene: turning an app's markup into entities, and keeping them in
//! step with its state.
//!
//! This is the half of a Lumen app that has no host in it. It spawns the
//! element tree from the IR the compiler produced, reconciles the parts of
//! that tree that depend on state (`<for>` rows, `<if>` branches, dialogs),
//! instantiates fragments, and resolves navigation between pages. What it
//! never does is lay out, paint, or talk to an OS: that is the host's half,
//! and it is why the same scene runs on a desktop window and in a browser
//! document.
//!
//! Nothing here registers itself. A host adds the systems it wants, in the
//! order its own pipeline needs; [`routing::install_routing`] is the one
//! exception, and it installs navigation as a unit because the resolver has
//! to run before the reconciler it drives.

#![warn(missing_docs)]

pub mod fragments;
pub mod routing;
pub mod spawn;
