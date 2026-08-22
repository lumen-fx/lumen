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
//! Most of this registers nothing. A host adds the systems it wants, in the
//! order its own pipeline needs. The two exceptions install as a unit because
//! their internal order is not the host's to choose:
//! [`routing::install_routing`], whose resolver has to run before the
//! reconciler it drives, and [`dom::install_dom`], whose applier has to run
//! after the collector that fills it.

#![warn(missing_docs)]

pub mod compiler_plugins;
pub mod dom;
pub mod fragments;
pub mod routing;
pub mod script_commands;
pub mod source_parser;
pub mod spawn;
