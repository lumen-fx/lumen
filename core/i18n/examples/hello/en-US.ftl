# Lumen i18n hello example - English source locale.

app-title = Hello World

# Greeting with one interpolated name. The `t!` macro maps a
# `name = "Alice"` Rust arg to `{ $name }` in the FTL message.
greet = Hello, { $name }!

# Plural form using Fluent's selector syntax. CLDR plural categories
# (`one`, `other` for English; `one`, `few`, `many`, `other` for
# Polish; etc.) are picked automatically by `fluent-bundle` based on
# `$count`.
items = { $count ->
    [one] { $count } item
   *[other] { $count } items
}
