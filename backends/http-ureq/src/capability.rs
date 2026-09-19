//! The `http-fetch` capability: the client the scripts' `fetch()` and
//! `http()` builtins run on.
//!
//! Installed as the `FetchRegistry` the script plugin would otherwise
//! create for itself, ahead of the script hosts. The plugin leaves an
//! existing registry alone, which is also how an embedder swaps in its own
//! `lumen_script::HttpClient` from an app hook or by inserting the resource
//! first. Costs nothing for an app that never fetches: no connection is
//! opened until a request is queued.

use std::sync::Arc;

use lumen_capability::{CapabilityEnv, Select};
use lumen_core::app::App;
use lumen_script::FetchRegistry;

use crate::UreqHttpClient;

/// What a static package looks for in the app's sources before it
/// carries this subsystem.
pub const SELECT: Select = Select::OnUse(&["fetch(", "http("]);

/// Install the subsystem. What the capability crate beside this one
/// registers.
pub fn install(app: &mut App, _env: &CapabilityEnv) {
    app.world
        .insert_resource(FetchRegistry::with_client(Arc::new(UreqHttpClient)));
}
