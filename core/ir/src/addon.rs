//! A browser add-on, as a compiled app carries it.
//!
//! An add-on is a package of files the browser loads beside the runtime: a
//! JavaScript module, and optionally stylesheets and a script that runs before
//! the page paints. What travels in the artifact is the part every target
//! needs to agree on, the functions it offers scripts and the elements it
//! answers for, so a compile, a build-time render and a desktop run all read
//! one description of it. The files themselves are the site's, and the site
//! manifest names them.
//!
//! Types are kept as the text the add-on's descriptor wrote (`int`,
//! `string[]`, `{string: any}`): this crate sits below the script layer that
//! reads them, and the spelling is the one a candela declaration uses anyway.

use serde::{Deserialize, Serialize};

/// One add-on an app depends on.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Addon {
    /// The name the app declared it under in `[dependencies]`.
    pub name: String,
    /// The script namespace its functions live in: `echo` makes a function
    /// `echo::shout` in candela.
    pub namespace: String,
    /// The functions it offers scripts, in the order its descriptor lists
    /// them.
    pub functions: Vec<AddonFunction>,
    /// The markup elements it answers for.
    pub elements: Vec<AddonElement>,
}

/// One function an add-on offers scripts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddonFunction {
    /// The name a script calls it by, and the name of the module export that
    /// answers.
    pub name: String,
    /// Its parameters, in order.
    pub params: Vec<AddonParam>,
    /// What it returns, as a type spelling. `null` for nothing, and always
    /// `null` for an asynchronous function, whose result arrives as an event.
    pub returns: String,
    /// The event an asynchronous function's result arrives as, or `None` for
    /// a function that answers before it returns.
    ///
    /// A script calls an asynchronous function with a tag after its declared
    /// arguments. When the module's promise settles, the script's `<event>`
    /// handler is called with the tag and the value, or `<event>_error` with
    /// the tag and the reason.
    pub event: Option<String>,
    /// One line describing it, for editor tooling.
    pub doc: String,
}

/// One declared parameter.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddonParam {
    /// Its name, for docs and error messages.
    pub name: String,
    /// Its type, as a type spelling.
    pub ty: String,
}

/// One markup element an add-on answers for.
///
/// The page writes it as the HTML element named here, with whatever the
/// markup put inside it as the content a visitor sees until the add-on takes
/// the element over, and on every target where it never does.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddonElement {
    /// The markup tag, such as `echo-view`.
    pub tag: String,
    /// The HTML element it is written as, such as `div` or `canvas`.
    pub html: String,
    /// True when that element takes no children and no end tag.
    pub void: bool,
}
