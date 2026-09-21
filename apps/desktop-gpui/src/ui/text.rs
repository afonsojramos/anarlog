use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

#[derive(Default)]
pub struct TextBuffer {
    pub text: String,
    pub anchor: usize,
    pub cursor: usize,
    pub marked: Option<Range<usize>>,
}

impl TextBuffer {
    pub fn selection(&self) -> Range<usize> {
        self.anchor.min(self.cursor)..self.anchor.max(self.cursor)
    }

    pub fn utf8(&self, offset: usize) -> usize {
        utf8(&self.text, offset)
    }

    pub fn utf16(&self, offset: usize) -> usize {
        self.text[..offset].encode_utf16().count()
    }

    pub fn previous(&self) -> usize {
        self.text
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .take_while(|i| *i < self.cursor)
            .last()
            .unwrap_or(0)
    }

    pub fn next(&self) -> usize {
        self.text
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .find(|i| *i > self.cursor)
            .unwrap_or(self.text.len())
    }

    pub fn replace(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        marked: Option<Range<usize>>,
    ) -> bool {
        let range = range
            .map(|range| self.utf8(range.start)..self.utf8(range.end))
            .or_else(|| self.marked.clone())
            .unwrap_or_else(|| self.selection());
        let text = text.replace(['\n', '\r'], " ");
        if self.text.len() - range.len() + text.len() > 4096 {
            return false;
        }
        self.text.replace_range(range.clone(), &text);
        if let Some(selection) = marked {
            self.marked = Some(range.start..range.start + text.len());
            self.anchor = range.start + utf8(&text, selection.start);
            self.cursor = range.start + utf8(&text, selection.end);
        } else {
            self.marked = None;
            self.cursor = range.start + text.len();
            self.anchor = self.cursor;
        }
        true
    }

    pub fn move_to(&mut self, offset: usize, extend: bool) {
        self.cursor = offset;
        if !extend {
            self.anchor = offset;
        }
    }
}

fn utf8(text: &str, offset: usize) -> usize {
    let mut units = 0;
    for (i, ch) in text.char_indices() {
        if units >= offset {
            return i;
        }
        units += ch.len_utf16();
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composition_ranges_are_relative_to_inserted_text() {
        let mut buffer = TextBuffer {
            text: "ab😀cd".into(),
            anchor: 6,
            cursor: 6,
            marked: None,
        };
        buffer.replace(None, "日本😀", Some(2..4));
        assert_eq!(buffer.selection(), 12..16);
        assert_eq!(buffer.marked, Some(6..16));
        buffer.replace(None, "日本語", None);
        assert_eq!(buffer.text, "ab😀日本語cd");
        assert_eq!(buffer.cursor, 15);
    }

    #[test]
    fn grapheme_delete_preserves_combining_sequences_and_utf16_offsets() {
        let mut buffer = TextBuffer {
            text: "e\u{301}😀".into(),
            anchor: 7,
            cursor: 7,
            marked: None,
        };
        assert_eq!(buffer.previous(), 3);
        assert_eq!(buffer.utf16(7), 4);
        buffer.anchor = buffer.previous();
        buffer.replace(None, "", None);
        assert_eq!(buffer.text, "e\u{301}");
        assert_eq!(buffer.previous(), 0);
    }
}
