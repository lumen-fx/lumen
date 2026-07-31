# Security policy

## Supported versions

Lumen has no tagged release yet. The latest `main` is the only supported
version; fixes land there.

## Reporting a vulnerability

Report privately through GitHub security advisories: open the Security tab of
this repository and use "Report a vulnerability". That opens a private thread
with the maintainers.

Do not open a public issue for a bug that is exploitable, including memory
unsafety across the C ABI, sandbox escapes from script hosts, and anything that
lets untrusted markup, styles, or scripts run code outside the app.

Include the version, the platform, and a repro if you have one. You get an
acknowledgement within a few days, and an update when the fix is ready or the
report is closed.
