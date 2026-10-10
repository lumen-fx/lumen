//! Where each line of a concatenated script came from.
//!
//! A language's program is every script file it owns plus the inline
//! `<script>` blocks, joined in source order into one text. A compiler reports
//! a line of that text; the map turns it back into the file the author wrote
//! and the line in it.

use serde::{Deserialize, Serialize};

/// One piece of a concatenated script.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourcePiece {
    /// The 1-based line of the concatenated text the piece starts on.
    pub at: u32,
    /// The file the piece was read from, as an error names it.
    pub file: String,
    /// The 1-based line of `file` the piece's first line is.
    pub first_line: u32,
}

/// The pieces of a concatenated script, in the order they were joined.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceMap {
    /// Every piece, ascending by [`SourcePiece::at`].
    pub pieces: Vec<SourcePiece>,
}

impl SourceMap {
    /// A map of a text read whole from `file`.
    pub fn whole(file: impl Into<String>) -> Self {
        Self {
            pieces: vec![SourcePiece {
                at: 1,
                file: file.into(),
                first_line: 1,
            }],
        }
    }

    /// Whether the map names no piece.
    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }

    /// Append `body`, which starts on line `first_line` of `file`, to `text`,
    /// separated from what `text` already holds by a newline, and record where
    /// it landed.
    pub fn append(&mut self, text: &mut String, body: &str, file: &str, first_line: u32) {
        if !text.is_empty() {
            text.push('\n');
        }
        let at = line_at(text, text.len());
        text.push_str(body);
        self.pieces.push(SourcePiece {
            at,
            file: file.to_owned(),
            first_line,
        });
    }

    /// Append the text `body` that `map` describes to `text`, the way
    /// [`Self::append`] appends one piece, keeping each of its pieces.
    pub fn append_mapped(&mut self, text: &mut String, body: &str, map: &SourceMap) {
        if !text.is_empty() {
            text.push('\n');
        }
        let offset = line_at(text, text.len()) - 1;
        text.push_str(body);
        self.pieces.extend(map.pieces.iter().map(|p| SourcePiece {
            at: p.at + offset,
            ..p.clone()
        }));
    }

    /// Give every piece recorded without a file the name `file`.
    pub fn name_unnamed(&mut self, file: &str) {
        for piece in self.pieces.iter_mut().filter(|p| p.file.is_empty()) {
            piece.file = file.to_owned();
        }
    }

    /// Count each piece's first line in `written`, the markup as the author
    /// wrote it, where the piece's text is found there as the body of an
    /// element. `text` is the concatenation this map describes.
    ///
    /// A parse sees markup with its `<include>` files spliced in, which shifts
    /// every line after an include; the author's file does not have them. A
    /// piece whose text the file does not hold (one an include brought in)
    /// keeps the line it was recorded with.
    pub fn place_in(&mut self, text: &str, written: &str) {
        let lines: Vec<&str> = text.split('\n').collect();
        let ends: Vec<u32> = self
            .pieces
            .iter()
            .skip(1)
            .map(|p| p.at - 1)
            .chain(std::iter::once(lines.len() as u32))
            .collect();
        for (piece, end) in self.pieces.iter_mut().zip(ends) {
            let from = (piece.at as usize).saturating_sub(1);
            let to = (end as usize).min(lines.len());
            if from >= to {
                continue;
            }
            let body = lines[from..to].join("\n");
            if body.trim().is_empty() {
                continue;
            }
            if let Some((found, _)) = written
                .match_indices(body.as_str())
                .find(|(i, _)| written[..*i].ends_with('>'))
            {
                piece.first_line = line_at(written, found);
            }
        }
    }

    /// The file and the line in it that line `line` of the concatenated text
    /// was read from. `None` when the map names no piece that early.
    pub fn locate(&self, line: u32) -> Option<(&str, u32)> {
        let piece = self.pieces.iter().rev().find(|p| p.at <= line)?;
        Some((piece.file.as_str(), piece.first_line + (line - piece.at)))
    }
}

/// The 1-based line byte offset `offset` of `text` is on.
pub fn line_at(text: &str, offset: usize) -> u32 {
    let end = offset.min(text.len());
    text.as_bytes()[..end]
        .iter()
        .filter(|b| **b == b'\n')
        .count() as u32
        + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appended_pieces_locate_back_to_their_files() {
        let mut text = String::new();
        let mut map = SourceMap::default();
        map.append(&mut text, "a1\na2", "main.lmn", 7);
        map.append(&mut text, "b1\nb2\nb3", "src/b.cdl", 1);
        assert_eq!(map.locate(1), Some(("main.lmn", 7)));
        assert_eq!(map.locate(2), Some(("main.lmn", 8)));
        assert_eq!(map.locate(3), Some(("src/b.cdl", 1)));
        assert_eq!(map.locate(5), Some(("src/b.cdl", 3)));
        assert_eq!(map.locate(0), None);
    }

    #[test]
    fn pieces_are_placed_at_their_line_in_the_written_file() {
        let written = "<root>\n<include src=\"a.lmn\"/>\n<script>\nlet x = 1;\n</script>\n</root>";
        // The spliced text carried two more lines ahead of the block.
        let mut text = String::new();
        let mut map = SourceMap::default();
        map.append(&mut text, "\nlet x = 1;\n", "", 5);
        map.name_unnamed("main.lmn");
        map.place_in(&text, written);
        assert_eq!(map.locate(2), Some(("main.lmn", 4)));
    }

    #[test]
    fn a_mapped_append_shifts_every_piece() {
        let mut inner_text = String::new();
        let mut inner = SourceMap::default();
        inner.append(&mut inner_text, "x", "a.lmn", 3);
        inner.append(&mut inner_text, "y", "b.lmn", 9);

        let mut text = String::new();
        let mut map = SourceMap::default();
        map.append(&mut text, "one\ntwo", "first.cdl", 1);
        map.append_mapped(&mut text, &inner_text, &inner);
        assert_eq!(text, "one\ntwo\nx\ny");
        assert_eq!(map.locate(3), Some(("a.lmn", 3)));
        assert_eq!(map.locate(4), Some(("b.lmn", 9)));
    }
}
