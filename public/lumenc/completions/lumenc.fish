# fish completion for lumenc.
#
# Install it where fish looks for completions:
#
#     lumenc completions fish > ~/.config/fish/completions/lumenc.fish
#
# `lumenc completions fish` prints this file.

# Every subcommand lumenc dispatches. Kept in step with `lumenc --help` by
# public/lumenc/tests/completions.rs.

complete -c lumenc -f

# --- top level ----------------------------------------------------------------

complete -c lumenc -n __fish_use_subcommand -a run -d 'Run an app'
complete -c lumenc -n __fish_use_subcommand -a check -d 'Parse an app without opening a window'
complete -c lumenc -n __fish_use_subcommand -a build -d 'Ahead-of-time compile an app to a .lmna artifact'
complete -c lumenc -n __fish_use_subcommand -a new -d 'Scaffold an app directory from a template'
complete -c lumenc -n __fish_use_subcommand -a fmt -d 'Reformat a .lmn markup file'
complete -c lumenc -n __fish_use_subcommand -a snapshot -d 'Text dump of the running app'
complete -c lumenc -n __fish_use_subcommand -a find -d 'Selector search over the live snapshot'
complete -c lumenc -n __fish_use_subcommand -a element-at -d 'Topmost element at a point'
complete -c lumenc -n __fish_use_subcommand -a click -d 'Inject a click'
complete -c lumenc -n __fish_use_subcommand -a type -d 'Type a string into the focused element'
complete -c lumenc -n __fish_use_subcommand -a key -d 'Inject one key press'
complete -c lumenc -n __fish_use_subcommand -a scroll -d 'Inject a wheel event'
complete -c lumenc -n __fish_use_subcommand -a lint -d 'Lint the running app, or lint sources offline'
complete -c lumenc -n __fish_use_subcommand -a diff -d 'Show what changed in the running app'
complete -c lumenc -n __fish_use_subcommand -a screenshot -d 'Capture the running app to a PNG'
complete -c lumenc -n __fish_use_subcommand -a web -d 'Emit the app as a static site'
complete -c lumenc -n __fish_use_subcommand -a bundle -d 'Pack an app, or build a trimmed runtime for it'
complete -c lumenc -n __fish_use_subcommand -a package -d 'Assemble a folder to ship'
complete -c lumenc -n __fish_use_subcommand -a i18n -d 'Translation catalogue tooling'
complete -c lumenc -n __fish_use_subcommand -a completions -d 'Print a shell completion script'
complete -c lumenc -n __fish_use_subcommand -s h -l help -d 'Show help'
complete -c lumenc -n __fish_use_subcommand -s V -l version -d 'Print version'

# Every subcommand answers --help.
complete -c lumenc -n 'not __fish_use_subcommand' -s h -l help -d 'Show help'

# --- run ----------------------------------------------------------------------

complete -c lumenc -n '__fish_seen_subcommand_from run' -a '(__fish_complete_directories)'
complete -c lumenc -n '__fish_seen_subcommand_from run' -l profile -x -a 'chrome tracy stderr' -d 'Write a trace or connect to a profiler'
complete -c lumenc -n '__fish_seen_subcommand_from run' -l headless -d 'Run the full pipeline with no window'
complete -c lumenc -n '__fish_seen_subcommand_from run' -l size -x -d 'Logical viewport, as WxH'
complete -c lumenc -n '__fish_seen_subcommand_from run' -l dpr -x -d 'Scale the offscreen target'
complete -c lumenc -n '__fish_seen_subcommand_from run' -l ticks -x -d 'Run exactly N ticks, then exit'
complete -c lumenc -n '__fish_seen_subcommand_from run' -l artifact -rF -d 'Run a precompiled artifact'
complete -c lumenc -n '__fish_seen_subcommand_from run' -l assets -rF -d 'Read assets from a .lpak archive'
complete -c lumenc -n '__fish_seen_subcommand_from run' -l no-hooks -d 'Skip the prebuild and prerun hooks'

# --- check --------------------------------------------------------------------

complete -c lumenc -n '__fish_seen_subcommand_from check' -a '(__fish_complete_directories)'

# --- build --------------------------------------------------------------------

complete -c lumenc -n '__fish_seen_subcommand_from build' -a '(__fish_complete_directories)'
complete -c lumenc -n '__fish_seen_subcommand_from build' -l no-hooks -d 'Skip the prebuild hooks'

# --- new ----------------------------------------------------------------------

complete -c lumenc -n '__fish_seen_subcommand_from new' -l list -s l -d 'Print the template gallery'
complete -c lumenc -n '__fish_seen_subcommand_from new' -a 'blank hello counter form todo dashboard settings hotkeys' -d 'Template'

# --- fmt ----------------------------------------------------------------------

complete -c lumenc -n '__fish_seen_subcommand_from fmt' -a '(__fish_complete_suffix .lmn)'
complete -c lumenc -n '__fish_seen_subcommand_from fmt' -l check -d 'Exit non-zero when the file would change'

# --- automation commands ------------------------------------------------------

set -l lumenc_mcp snapshot find element-at click type key scroll lint diff screenshot
complete -c lumenc -n "__fish_seen_subcommand_from $lumenc_mcp" -l port -x -d 'MCP port'
complete -c lumenc -n "__fish_seen_subcommand_from $lumenc_mcp" -l app -xa '(__fish_complete_directories)' -d 'App directory to read the MCP port from'

set -l lumenc_json snapshot find element-at click type key scroll lint diff
complete -c lumenc -n "__fish_seen_subcommand_from $lumenc_json" -l json -d 'Print the raw JSON-RPC result'

set -l lumenc_simulate click type key scroll
complete -c lumenc -n "__fish_seen_subcommand_from $lumenc_simulate" -l wait-for -x -d 'Block until this event ring records an entry'

complete -c lumenc -n '__fish_seen_subcommand_from snapshot' -l text -d 'Indented text tree (default)'
complete -c lumenc -n '__fish_seen_subcommand_from snapshot' -l max-lines -x -d 'Stop after N lines'
complete -c lumenc -n '__fish_seen_subcommand_from snapshot' -l cursor -x -d 'Resume a truncated dump'
complete -c lumenc -n '__fish_seen_subcommand_from snapshot' -l include-invisible -d 'Include elements the app is not painting'
complete -c lumenc -n '__fish_seen_subcommand_from snapshot' -l no-omit-invisible -d 'Alias for --include-invisible'

complete -c lumenc -n '__fish_seen_subcommand_from find' -l text -x -d 'Match elements whose label contains this'
complete -c lumenc -n '__fish_seen_subcommand_from find' -l role -x -d 'Match elements with this a11y role'
complete -c lumenc -n '__fish_seen_subcommand_from find' -l id -x -d 'Match one entity id'
complete -c lumenc -n '__fish_seen_subcommand_from find' -l limit -x -d 'Stop after N hits'

complete -c lumenc -n '__fish_seen_subcommand_from click' -l button -x -a 'primary secondary middle' -d 'Mouse button'

complete -c lumenc -n '__fish_seen_subcommand_from key' -l shift -d 'Hold shift'
complete -c lumenc -n '__fish_seen_subcommand_from key' -l ctrl -d 'Hold ctrl'
complete -c lumenc -n '__fish_seen_subcommand_from key' -l alt -d 'Hold alt'
complete -c lumenc -n '__fish_seen_subcommand_from key' -l super -d 'Hold super'
complete -c lumenc -n '__fish_seen_subcommand_from key' -l cmd -d 'Alias for --super'
complete -c lumenc -n '__fish_seen_subcommand_from key' -a 'Enter Tab Escape Backspace Delete Home End Left Right Up Down' -d 'Key name'

complete -c lumenc -n '__fish_seen_subcommand_from lint' -a '(__fish_complete_directories)'
complete -c lumenc -n '__fish_seen_subcommand_from lint' -l css-cascade -d 'Offline cascade-divergence check'
complete -c lumenc -n '__fish_seen_subcommand_from lint' -l signals -d 'Offline signal lint'
complete -c lumenc -n '__fish_seen_subcommand_from lint' -l strict -d 'Upgrade warnings to errors'

complete -c lumenc -n '__fish_seen_subcommand_from screenshot' -F
complete -c lumenc -n '__fish_seen_subcommand_from screenshot' -l highlight -x -d 'Outline these entity ids'
complete -c lumenc -n '__fish_seen_subcommand_from screenshot' -l lint -d 'Outline every lint finding'
complete -c lumenc -n '__fish_seen_subcommand_from screenshot' -l bounds -rF -d 'Also write the entity bounds map as JSON'

# --- web ---------------------------------------------------------------------

complete -c lumenc -n '__fish_seen_subcommand_from web' -a '(__fish_complete_directories)'
complete -c lumenc -n '__fish_seen_subcommand_from web' -l out -xa '(__fish_complete_directories)' -d 'Where the site is written'
complete -c lumenc -n '__fish_seen_subcommand_from web' -l base -x -d 'URL prefix the site is served under'
complete -c lumenc -n '__fish_seen_subcommand_from web' -l locale -x -d 'Locale to emit a document tree for'
complete -c lumenc -n '__fish_seen_subcommand_from web' -l render -x -a 'static csr ssr' -d 'Where the pages come from'
complete -c lumenc -n '__fish_seen_subcommand_from web' -l runtime -d 'Put the browser runtime in the documents'
complete -c lumenc -n '__fish_seen_subcommand_from web' -l no-runtime -d 'Leave the browser runtime out'
complete -c lumenc -n '__fish_seen_subcommand_from web' -l prerender -x -a 'seeds run none' -d 'Where the rendered state comes from'
complete -c lumenc -n '__fish_seen_subcommand_from web' -l no-hooks -d 'Skip the prebuild hooks'
complete -c lumenc -n '__fish_seen_subcommand_from web' -l lib-dir -xa '(__fish_complete_directories)' -d 'Directory holding the browser runtime'
complete -c lumenc -n '__fish_seen_subcommand_from web' -l strict -d 'Fail the build on any warning'
complete -c lumenc -n '__fish_seen_subcommand_from web' -l serve -d 'Serve the site and print the URL'
complete -c lumenc -n '__fish_seen_subcommand_from web' -l port -x -d 'Port to serve on'
complete -c lumenc -n '__fish_seen_subcommand_from web' -l host -x -d 'Address to listen on'
complete -c lumenc -n '__fish_seen_subcommand_from web' -l allow-host -x -d 'Let a render ask this host for data'

# --- bundle -------------------------------------------------------------------

complete -c lumenc -n '__fish_seen_subcommand_from bundle' -a '(__fish_complete_directories)'
complete -c lumenc -n '__fish_seen_subcommand_from bundle' -l static -d 'Build a trimmed runtime for the app'
complete -c lumenc -n '__fish_seen_subcommand_from bundle' -l no-hooks -d 'Skip the prebuild hooks'

# --- package ------------------------------------------------------------------

complete -c lumenc -n '__fish_seen_subcommand_from package' -a '(__fish_complete_directories)'
complete -c lumenc -n '__fish_seen_subcommand_from package' -l name -x -d 'Package name'
complete -c lumenc -n '__fish_seen_subcommand_from package' -l target -x -a 'linux-x86_64 linux-aarch64 macos-x86_64 macos-aarch64 windows-x86_64' -d 'Platform to package for'
complete -c lumenc -n '__fish_seen_subcommand_from package' -l lib-dir -xa '(__fish_complete_directories)' -d 'Directory holding the platform files'
complete -c lumenc -n '__fish_seen_subcommand_from package' -l no-hooks -d 'Skip the prebuild hooks'

# --- i18n ---------------------------------------------------------------------

complete -c lumenc -n '__fish_seen_subcommand_from i18n; and not __fish_seen_subcommand_from extract' -a extract -d 'Extract translation keys'
complete -c lumenc -n '__fish_seen_subcommand_from i18n; and __fish_seen_subcommand_from extract' -a '(__fish_complete_directories)'
complete -c lumenc -n '__fish_seen_subcommand_from i18n' -l lang -x -d 'BCP-47 tag naming the catalogue to write'

# --- completions --------------------------------------------------------------

complete -c lumenc -n '__fish_seen_subcommand_from completions' -a 'bash zsh fish' -d 'Shell'
