# `@keyframes` animations run on the desktop

This affects desktop apps whose stylesheet has `@keyframes` blocks or
`animation` properties, which used to reach only the web target.

The desktop now plays them: an element with `animation: spin 2s infinite`
spins in a window as it does in a browser, and raises `animationstart`,
`animationiteration`, `animationend` and `animationcancel` events. An app that
wrote an animation for its web build only and did not want it on the desktop
turns it off there with a rule only the desktop reads, or moves the
`animation` declaration into a stylesheet only the web build includes.

`@media (prefers-reduced-motion: reduce)` now matches when the OS asks for
less motion, so rules inside it apply on the desktop where they never did.
