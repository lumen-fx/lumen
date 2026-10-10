# A plugin added with `add_plugin` builds before the markup is parsed

This affects Rust apps that add plugins through the Rust SDK's
`App::add_plugin` or `RunOptions::with_plugin`. Those plugins now build right
after the app's runtime modules install, before the markup is parsed, where
they used to build after it. A widget plugin's tag is therefore registered in
time for the markup that uses it, with no `register()` call beforehand.

A plugin whose `build` read a resource the runtime adds while it sets up the
document, such as the scripting DOM's command queue, finds it missing now.
Read it from a system the plugin adds instead: systems run once the app is
built, when everything the runtime installs is there.
