//! Turning arbitrary strings into safe HTML.
//!
//! Text and attribute values need different sets: a quote is only dangerous
//! inside a quoted attribute, and an ampersand is dangerous in both. Both
//! functions borrow when there is nothing to escape, which is the common
//! case.

use std::borrow::Cow;

/// Escape a string for use as text between tags.
pub fn escape_text(input: &str) -> Cow<'_, str> {
    escape(input, |c| matches!(c, '&' | '<' | '>'))
}

/// Escape a string for use as a double-quoted attribute value.
///
/// Single quotes are escaped too, so the same output is also safe if a
/// consumer ever writes it inside single quotes.
pub fn escape_attr(input: &str) -> Cow<'_, str> {
    escape(input, |c| matches!(c, '&' | '<' | '>' | '"' | '\''))
}

fn escape(input: &str, needs: fn(char) -> bool) -> Cow<'_, str> {
    if !input.contains(needs) {
        return Cow::Borrowed(input);
    }
    let mut out = String::with_capacity(input.len() + 16);
    for c in input.chars() {
        match c {
            '&' if needs('&') => out.push_str("&amp;"),
            '<' if needs('<') => out.push_str("&lt;"),
            '>' if needs('>') => out.push_str("&gt;"),
            '"' if needs('"') => out.push_str("&quot;"),
            '\'' if needs('\'') => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_borrowed_unchanged() {
        assert!(matches!(escape_text("plain text"), Cow::Borrowed(_)));
        assert_eq!(escape_text("plain text"), "plain text");
        assert!(matches!(escape_attr("plain-value"), Cow::Borrowed(_)));
    }

    #[test]
    fn text_escapes_markup_delimiters() {
        assert_eq!(
            escape_text("a < b && c > d"),
            "a &lt; b &amp;&amp; c &gt; d"
        );
        assert_eq!(escape_text("</script>"), "&lt;/script&gt;");
    }

    #[test]
    fn text_leaves_quotes_alone() {
        assert_eq!(escape_text(r#"say "hi""#), r#"say "hi""#);
    }

    #[test]
    fn attributes_escape_both_quote_styles() {
        assert_eq!(
            escape_attr(r#"a" onload="alert(1)"#),
            "a&quot; onload=&quot;alert(1)"
        );
        assert_eq!(escape_attr("it's"), "it&#39;s");
        assert_eq!(escape_attr("a & b"), "a &amp; b");
    }

    #[test]
    fn attributes_escape_every_special_at_once() {
        assert_eq!(
            escape_attr(r#"<a href="x">&'"#),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39;"
        );
    }
}
