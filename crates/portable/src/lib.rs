//! The Lumen app every platform starts from.
//!
//! An app is the same app in a window, in a page and on a server: the same
//! widget behaviour, the same reconcilers, the same two-way bindings, the same
//! script hosts. What differs is layout, paint, windowing and the OS surface,
//! and none of that is here. This crate assembles the part that does not
//! differ, so a platform adds its own backends to it rather than restating it.
//!
//! [`portable_app`] builds that app, [`hosts::install`] puts the host for an
//! engine into it, and [`apply_seed`] applies the state a rendered document
//! was produced from.
//!
//! Nothing here is `!Send`, which is what makes the assembly usable from a
//! thread that is not the process's main one: bevy's non-send resources may
//! only be reached, and dropped, from the thread that inserted them.

#![warn(missing_docs)]

pub mod assemble;
pub mod hosts;

pub use assemble::{apply_seed, portable_app};
