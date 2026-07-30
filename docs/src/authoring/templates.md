# Templates + slots

`<template name="…">` defines a reusable markup body. Slots, id
auto-namespacing, and default attribute values are collectively enough
to refactor any repeated block out of a `.lmn` file and keep
per-instance state isolated.

This chapter walks the surface end-to-end: a before/after refactor,
the core features, and the deferred work.

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

`<template name="X" {default-attrs}>…body…</template>` defines a
reusable subtree. Defaults are key="value" pairs on the `<template>`
tag itself.

```xml
<template name="card" variant="primary" size="md">
  <column class="card card-{variant} card-{size}">
    <slot />
  </column>
</template>
```

- `name="card"` — the use-site tag name.
- `variant="primary"` and `size="md"` — defaults. Use-sites that omit
  them inherit; use-sites that pass `variant="danger"` override.
- `{variant}` and `{size}` inside the body — textual placeholders.
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

<!-- Explicit form — useful when the template name shadows something. -->
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

`<slot default="…">` falls back to the literal text when the use-site
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

The matching Rhai routing handles both shapes:

```rhai
// Fires for BOTH user-card:save AND team-card:save via suffix fallback.
on("click", "save", "do_save");

// Per-instance routing — only fires for the user-card instance.
on("click", "user-card:save", "do_user_save");

fn do_save(id) {
    // `id` is the fully-qualified id; use local_id to find siblings.
    let label_id = local_id(id, "status");
    set_text(label_id, "Saved!");
}
```

`local_id(source, suffix)` returns a sibling id within the same
template instance. Multi-level prefixes (`a:b:save`) stack — the result
is `a:b:status`. Source ids without a colon (top-level entities)
return `suffix` unchanged.

## Defaults vs use-site overrides

```xml
<template name="banner" tone="info" closable="false">
  <row class="banner banner-{tone}">
    <label grow="1" text="{message}" />
    <button id="close" text="×" />
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
   first `<banner>` would render `text=""` — that's a parse choice,
   not a parse error. Author defensively.
3. **Defaults stack with placeholders.** `class="banner banner-{tone}"`
   resolves with the chosen `tone` per-instance.

## Slot content

The slot content is the use-site's *inline children*. Attributes on the
use-site tag itself flow through the placeholder substitution; the
inner element tree slots in where `<slot/>` lives.

```xml
<template name="dialog">
  <overlay class="modal-dim">
    <column class="modal-body" width="420">
      <slot />
    </column>
  </overlay>
</template>

<dialog>
  <label text="Are you sure?" />
  <row gap="10">
    <button id="cancel" text="Cancel" />
    <button id="confirm" text="OK" />
  </row>
</dialog>
```

The two children of `<dialog>` land where `<slot/>` was. There's only
**one slot per template** today — named slots (`<slot name="header"/>`)
are on the deferred list.

## A bigger refactor — list items

Before:

```xml
<for each="todos" key="id">
  <row class="list-row" align="center" gap="10">
    <label width="32" text="{row.idx}" />
    <label grow="1" text="{row.label}" />
    <label class="pill" text="{row.status}" />
    <button id="dismiss-{row.id}" text="×" />
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
    <button id="dismiss" text="×" />
  </row>
</template>

<for each="todos" key="id">
  <todo-row id="row-{row.id}" idx="{row.idx}" label="{row.label}" status="{row.status}" />
</for>
```

```rhai
// Auto-namespacing: dismiss button ids are row-1:dismiss, row-2:dismiss, …
on("click", "dismiss", "handle_dismiss");

fn handle_dismiss(id) {
    // local_id(id, "label") = row-3:label etc.
    let row_id = id.split(":")[0];   // e.g. "row-3"
    // … remove the row from todos signal_array …
}
```

Two wins: the markup is denser, and the `dismiss` handler is one
function regardless of how many rows exist.

## Combining with `<for>`

`<for each>` row-field substitution (`{row.label}`) and `<template>`
attribute substitution (`{tone}`) use the same expansion rule and
compose cleanly. `<for>` evaluates first (per-row item context), then
the template expansion runs on the substituted use-site tag.

## Deferred features

These are not shipped yet:

- **Named slots** (`<slot name="header"/>` and
  `<header slot="header">…</header>` at the use-site) for multi-slot
  layouts like cards with separate title + body + footer sections.
- **Scoped signals** — today `signal("count")` is global. Per-instance
  signals scoped to a template instance would let two `<counter />`
  uses keep independent state without the author writing
  `signal("count-" + instance_id)` manually.
- **`on_mount(id, fn)` / `on_unmount(id, fn)`** lifecycle hooks.
- **`:host` CSS pseudo-class** to target the template root element
  from the template's own stylesheet.

> **Limitation: placeholder substitution is textual.** `{variant}`
> resolves through a literal string replace at parse time, so
> placeholders inside attribute values that need careful escaping
> (e.g. `text="Click for {kind} info"` where `{kind}` contains `<`)
> currently inherit XML's escaping rules. The widget-garden app's
> template demos use only safe characters as a workaround.
