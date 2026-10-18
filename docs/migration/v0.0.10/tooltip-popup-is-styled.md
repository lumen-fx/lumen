# The tooltip popup takes the app's stylesheet

This affects desktop apps with a `.tooltip` rule or a `tooltip` type
selector, and apps whose root sets a font or a text color.

The popup a `<tooltip>` opens used to be styled by the runtime alone, so a
`.tooltip` rule in an app's stylesheet never reached it. It is now a
`tooltip` element with the class `tooltip` under the root, and the cascade
styles it like any element: an app's `.tooltip` rule restyles it over the
skin's look, and `var()` tokens declared on the root resolve in it. A rule
written for tooltips that did nothing before now applies.

The popup inherits from the root, so a font family set on the root reaches
tooltip text. Its default size stays 13 px, and its unskinned look (a dark
panel with light text) now comes from the built-in user-agent sheet, which
any skin or app rule for `.tooltip` outranks.

## Rust

`lumen_primitives::TooltipPopup` entities now carry `LumenTag`, `DomHidden`
and a `StyleOverrides` that holds their `position` and `inset`, and are
children of the root of the trigger's tree. The runtime no longer inserts a
fallback `Visuals` and `TextStyle` on them. Code that wrote a popup's
`Style`, `Visuals` or `TextStyle` directly loses those writes at the next
restyle; hold the value in `StyleOverrides` instead.
