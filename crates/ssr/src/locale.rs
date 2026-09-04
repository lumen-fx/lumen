//! Which of the languages a site holds a visitor asked for.
//!
//! `Accept-Language` is a list of ranges with a weight each, and matching one
//! against a set of tags is RFC 4647 basic filtering: a range matches a tag it
//! equals, or one that continues it at a subtag boundary. What is added on top
//! is a second pass on the primary subtag alone, so a visitor asking for
//! `de-AT` reaches a site that only holds `de-DE` rather than falling back to
//! a language they did not ask for.

/// The tag from `available` the header asks for first, or `None` when it asks
/// for none of them.
///
/// `available[0]` is the site's default, which is what `*` selects.
pub(crate) fn negotiate<'a>(header: &str, available: &[&'a str]) -> Option<&'a str> {
    let ranges = ranges(header);
    // Every range at its weight is tried by the stricter rule before any of
    // them is tried by the looser one, so a tag the visitor named exactly wins
    // over a language match on a range they weighted lower.
    for range in &ranges {
        if range == "*" {
            return available.first().copied();
        }
        if let Some(tag) = available.iter().find(|tag| basic_match(range, tag)) {
            return Some(tag);
        }
    }
    for range in &ranges {
        let asked = primary(range);
        if let Some(tag) = available.iter().find(|tag| primary(tag) == asked) {
            return Some(tag);
        }
    }
    None
}

/// The header's ranges, most wanted first. A range weighted `q=0` is one the
/// visitor refused, so it is dropped rather than ranked last.
fn ranges(header: &str) -> Vec<String> {
    let mut weighted: Vec<(usize, f32, String)> = Vec::new();
    for (order, part) in header.split(',').enumerate() {
        let mut fields = part.split(';');
        let range = fields.next().unwrap_or("").trim();
        if range.is_empty() {
            continue;
        }
        let quality = fields
            .filter_map(|field| field.trim().strip_prefix("q=")?.trim().parse::<f32>().ok())
            .next()
            .unwrap_or(1.0);
        if quality <= 0.0 {
            continue;
        }
        weighted.push((order, quality, range.to_ascii_lowercase()));
    }
    // Equal weights keep the order they were written in, which is the only
    // preference the visitor has left to express between them.
    weighted.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    weighted.into_iter().map(|(_, _, range)| range).collect()
}

/// RFC 4647 basic filtering: the tag is the range, or continues it at a
/// subtag boundary. `range` is already lowercase.
fn basic_match(range: &str, tag: &str) -> bool {
    let tag = tag.to_ascii_lowercase();
    tag == range
        || tag
            .strip_prefix(range)
            .is_some_and(|rest| rest.starts_with('-'))
}

/// The language a tag or a range starts with, lowercased.
fn primary(tag: &str) -> String {
    tag.split('-').next().unwrap_or("").to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELD: &[&str] = &["en-US", "de-DE", "ja-JP"];

    #[test]
    fn the_tag_asked_for_at_the_highest_weight_wins() {
        assert_eq!(negotiate("de-DE,de;q=0.9,en;q=0.8", HELD), Some("de-DE"));
        assert_eq!(negotiate("en;q=0.8,ja-JP;q=0.9", HELD), Some("ja-JP"));
        // Equal weights are read in the order they were written.
        assert_eq!(negotiate("ja,de", HELD), Some("ja-JP"));
    }

    #[test]
    fn a_range_matches_a_tag_that_continues_it() {
        assert_eq!(negotiate("de", HELD), Some("de-DE"));
        assert_eq!(negotiate("DE-de", HELD), Some("de-DE"));
        // A region the site holds no tree for still reaches the language.
        assert_eq!(negotiate("de-AT", HELD), Some("de-DE"));
        // And the boundary is a subtag, not a prefix.
        assert_eq!(negotiate("d", HELD), None);
    }

    #[test]
    fn a_wildcard_takes_the_default_tree() {
        assert_eq!(negotiate("*", HELD), Some("en-US"));
        assert_eq!(negotiate("fr;q=0.9,*;q=0.1", HELD), Some("en-US"));
    }

    #[test]
    fn a_refused_range_is_not_a_preference() {
        assert_eq!(negotiate("de;q=0", HELD), None);
        assert_eq!(negotiate("de;q=0,ja;q=0.5", HELD), Some("ja-JP"));
        assert_eq!(negotiate("*;q=0", HELD), None);
    }

    #[test]
    fn a_header_asking_for_nothing_the_site_holds_matches_nothing() {
        assert_eq!(negotiate("fr-FR,fr;q=0.9", HELD), None);
        assert_eq!(negotiate("", HELD), None);
        assert_eq!(negotiate("de-DE", &[]), None);
    }
}
