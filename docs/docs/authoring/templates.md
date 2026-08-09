# Templates + slots

`<template name="...">` defines a reusable markup body. Together with
slots, id auto-namespacing, and default attribute values, that is enough
to factor any repeated block out of a `.lmn` file and keep each instance's
state separate, without introducing a component lifecycle.

## Why templates?

Without templates, repeated UI looks like this:

```xml
<column gap="12">
  <column class="card card-primary">
    <label class="card-title" text="User profile" />
    <label class="card-body"  text="Edit your name and avatar." />
  </column>
  <column class="card card-primary">
    <label class="card-title" text="Team settings" />
    <label class="card-body"  text="Configure shared resources." />
  </column>
  <column class="card card-danger">
    <label class="card-title" text="Danger zone" />
    <label class="card-body"  text="Permanent destructive actions." />
  </column>
</column>
```

Templates cut the repetition without introducing a component lifecycle
or a custom view-model:

```xml
<template name="card" variant="primary">
  <column class="card card-{variant}">
    <slot />
  </column>
</template>

<column gap="12">
  <card>
    <label class="card-title" text="User profile" />
    <label class="card-body"  text="Edit your name and avatar." />
  </card>
  <card>
    <label class="card-title" text="Team settings" />
    <label class="card-body"  text="Configure shared resources." />
  </card>
  <card variant="danger">
    <label class="card-title" text="Danger zone" />
    <label class="card-body"  text="Permanent destructive actions." />
  </card>
</column>
```

Three features are doing work here: `<slot/>` (the inner content
substitution point), `{variant}` placeholder (per-use-site attribute
substitution), and the `variant="primary"` default (kicked in for the
first two `<card>`s that didn't override it).

## Defining a template

`<template name="X" {default-attrs}>...body...</template>` defines a
reusable subtree. Defaults are key="value" pairs on the `<template>`
tag itself.

```xml
<template name="card" variant="primary" size="md">
  <column class="card card-{variant} card-{size}">
    <slot />
  </column>
</template>
```

- `name="card"` - the use-site tag name.
- `variant="primary"` and `size="md"` - defaults. Use-sites that omit
  them inherit; use-sites that pass `variant="danger"` override.
- `{variant}` and `{size}` inside the body - textual placeholders.
  At expand-time the parser substitutes the resolved value (use-site
  override > template default).

Templates may live anywhere in the file. The parser collects them in a
first pass before walking the layout tree, so use-sites can appear
before the definition.

## Using a template

Two equivalent forms:

```xml
<!-- Tag name = template name. -->
<card variant="danger">
  <label text="Heads up" />
</card>

<!-- Explicit form - useful when the template name shadows something. -->
<use template="card" variant="danger">
  <label text="Heads up" />
</use>
```

Self-closing uses get no slot content:

```xml
<card />     <!-- the <slot/> renders nothing -->
```

Pair with a default for graceful empty:

```xml
<template name="card">
  <column class="card">
    <slot default="(empty)" />
  </column>
</template>
```

`<slot default="...">` falls back to the literal text when the use-site
provides no child elements.

## Id auto-namespacing

When a template instance gets an `id`, inner ids stack:

```xml
<template name="actions">
  <row>
    <button id="save"   text="Save" />
    <button id="cancel" text="Cancel" />
  </row>
</template>

<actions id="user-card" />
<actions id="team-card" />
```

The first instance spawns buttons with ids `user-card:save` and
`user-card:cancel`; the second instance produces `team-card:save` and
`team-card:cancel`. The colon is the namespace delimiter.

Handler routing works on either shape:

```candela
fn on_start() {
    // Fires for both user-card:save and team-card:save via suffix fallback.
    lumen::on("click", "save", "do_save");

    // Per-instance routing: only the user-card instance.
    lumen::on("click", "user-card:save", "do_user_save");
}

fn do_save(id) {
    // `id` is the fully-qualified id, so a sibling is the same prefix
    // with a different suffix.
    let parts = id.split(":");
    let status_id = parts[0] + ":status";
    lumen::set_text(status_id, "Saved!");
}
```

A handler registered for the bare suffix matches every instance; register the
qualified id (`"user-card:save"`) to target one. Multi-level prefixes
(`a:b:save`) stack, and the suffix is whatever follows the last colon.

## Defaults vs use-site overrides

```xml
<template name="banner" tone="info" closable="false">
  <row class="banner banner-{tone}">
    <label grow="1" text="{message}" />
    <button id="close" text="x" />
  </row>
</template>

<banner message="Build started." />
<banner message="Build failed."  tone="danger" />
<banner message="Login expires soon." tone="warn" closable="true" />
```

Three rules:

1. **Use-site wins.** `<banner tone="danger">` overrides the
   `tone="info"` default for that one instance.
2. **Missing default = empty.** `message` has no default, so the
   first `<banner>` would render `text=""` - that's a parse choice,
   not a parse error. Author defensively.
3. **Defaults stack with placeholders.** `class="banner banner-{tone}"`
   resolves with the chosen `tone` per-instance.

## Slot content

The slot content is the use-site's *inline children*. Attributes on the
use-site tag itself flow through the placeholder substitution; the
inner element tree slots in where `<slot/>` lives.

```xml
<template name="modal">
  <overlay class="modal-dim">
    <column class="modal-body" width="420">
      <slot />
    </column>
  </overlay>
</template>

<modal>
  <label text="Are you sure?" />
  <row gap="10">
    <button id="cancel" text="Cancel" />
    <button id="confirm" text="OK" />
  </row>
</modal>
```

The two children of `<modal>` land where `<slot/>` was. A template has one
slot; named slots are not supported.

Pick a template name that no built-in tag already uses. A `<template
name="dialog">` would shadow the real `<dialog>` tag for the whole app.

## A bigger refactor - list items

Before:

```xml
<for each="todos" key="id">
  <row class="list-row" align="center" gap="10">
    <label width="32" text="{row.idx}" />
    <label grow="1" text="{row.label}" />
    <label class="pill" text="{row.status}" />
    <button id="dismiss-{row.id}" text="x" />
  </row>
</for>
```

After:

```xml
<template name="todo-row">
  <row class="list-row" align="center" gap="10">
    <label width="32" text="{idx}" />
    <label grow="1" text="{label}" />
    <label class="pill" text="{status}" />
    <button id="dismiss" text="x" />
  </row>
</template>

<for each="todos" key="id">
  <todo-row id="row-{row.id}" idx="{row.idx}" label="{row.label}" status="{row.status}" />
</for>
```

```candela
// Auto-namespacing: dismiss button ids are row-1:dismiss, row-2:dismiss, ...
fn on_start() {
    lumen::on("click", "dismiss", "handle_dismiss");
}

fn handle_dismiss(id) {
    let parts = id.split(":");
    let row_id = parts[0];           // e.g. "row-3"
    // ... drop that row from the list ...
}
```

Two wins: the markup is denser, and the `dismiss` handler is one
function regardless of how many rows exist.

## Combining with `<for>`

`<for each>` row-field substitution (`{row.label}`) and `<template>`
attribute substitution (`{tone}`) use the same expansion rule and
compose cleanly. `<for>` evaluates first (per-row item context), then
the template expansion runs on the substituted use-site tag.

## Limits

Placeholder substitution is textual. `{variant}` resolves through a
literal string replace at parse time, so a value that needs XML escaping
(`text="Click for {kind} info"` where `{kind}` contains a `<`) breaks the
document. Keep placeholder values to plain text.

A template has one slot, and it takes the use-site's whole child list.
There are no named slots, so a card with separate title, body, and footer
regions needs three templates or an attribute per region.

Signals are global. Two `<counter />` instances share `signal("count")`
unless you namespace the name yourself, the way ids namespace
automatically. There are no per-instance mount and unmount hooks, and no
`:host` selector for styling a template's root from inside the template.
