# Security policy

## Supported versions

Lumen is in alpha. Only the latest release is supported: fixes land on `main`
and ship in the next tagged release. Older tags get no backports, so upgrade
before reporting a problem you found on one.

## Reporting a vulnerability

Report privately through GitHub security advisories: open the Security tab of
this repository and use "Report a vulnerability". That opens a private thread
with the maintainers.

Do not open a public issue for a bug that is exploitable, including memory
unsafety across the C ABI, sandbox escapes from a script host (candela, Rhai,
or Lua), and anything that lets untrusted markup, styles, or scripts run code
outside the app.

Include the version (`lumenc --version`), the platform, and a repro if you have
one. You get an acknowledgement within a few days, and an update when the fix
is ready or the report is closed.
