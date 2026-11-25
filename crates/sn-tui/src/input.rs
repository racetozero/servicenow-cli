//! One text editor for every field: single line or several lines.

use crate::text;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Editor {
    pub text: String,
    /// The cursor, as a byte position on a character boundary.
    pub cursor: usize,
    pub multiline: bool,
}

/// Text laid out for drawing: the visual lines and where the cursor is.
pub struct Visual {
    pub lines: Vec<String>,
    pub row: usize,
    pub column: usize,
}

impl Editor {
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            cursor: text.len(),
            text,
            multiline: false,
        }
    }

    pub fn multiline(text: impl Into<String>) -> Self {
        Self {
            multiline: true,
            ..Self::new(text)
        }
    }

    pub fn insert(&mut self, text: &str) {
        let text: String = match self.multiline {
            true => text.replace("\r\n", "\n").replace('\r', "\n"),
            false => text
                .trim_end_matches(['\r', '\n'])
                .replace(['\r', '\n'], " "),
        };
        self.text.insert_str(self.cursor, &text);
        self.cursor += text.len();
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    fn before(&self) -> usize {
        self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(at, _)| at)
    }

    fn after(&self) -> usize {
        self.text[self.cursor..]
            .chars()
            .next()
            .map_or(self.cursor, |c| self.cursor + c.len_utf8())
    }

    fn line_start(&self) -> usize {
        self.text[..self.cursor].rfind('\n').map_or(0, |at| at + 1)
    }

    fn line_end(&self) -> usize {
        self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |at| self.cursor + at)
    }

    fn word_start(&self) -> usize {
        let head = self.text[..self.cursor].trim_end();
        head.rfind(char::is_whitespace).map_or(0, |at| {
            at + head[at..].chars().next().map_or(0, char::len_utf8)
        })
    }

    fn word_end(&self) -> usize {
        let tail = &self.text[self.cursor..];
        let skipped = tail.len() - tail.trim_start().len();
        tail[skipped..]
            .find(char::is_whitespace)
            .map_or(self.text.len(), |at| self.cursor + skipped + at)
    }

    /// Edits with `key`. False when the key is not an edit, so the caller
    /// can use it.
    pub fn key(&mut self, key: KeyEvent) -> bool {
        // Windows reports AltGr as Ctrl+Alt, and AltGr types `@` and `\` on
        // many layouts, so both together count as neither.
        let (ctrl, alt) = (
            key.modifiers.contains(KeyModifiers::CONTROL),
            key.modifiers.contains(KeyModifiers::ALT),
        );
        let (ctrl, alt) = (ctrl && !alt, alt && !ctrl);
        match key.code {
            KeyCode::Char('a') if ctrl => self.cursor = self.line_start(),
            KeyCode::Char('e') if ctrl => self.cursor = self.line_end(),
            KeyCode::Char('u') if ctrl => {
                let start = self.line_start();
                self.text.replace_range(start..self.cursor, "");
                self.cursor = start;
            }
            KeyCode::Char('w') if ctrl => self.delete_word(),
            KeyCode::Backspace if ctrl || alt => self.delete_word(),
            KeyCode::Char('b') if alt => self.cursor = self.word_start(),
            KeyCode::Char('f') if alt => self.cursor = self.word_end(),
            KeyCode::Left if ctrl || alt => self.cursor = self.word_start(),
            KeyCode::Right if ctrl || alt => self.cursor = self.word_end(),
            KeyCode::Char('j') if ctrl && self.multiline => self.insert("\n"),
            KeyCode::Char(c) if !ctrl && !alt && !c.is_control() => {
                self.insert(c.encode_utf8(&mut [0; 4]));
            }
            KeyCode::Enter if self.multiline && !ctrl && !alt => self.insert("\n"),
            KeyCode::Backspace => {
                let start = self.before();
                self.text.replace_range(start..self.cursor, "");
                self.cursor = start;
            }
            KeyCode::Delete => {
                let end = self.after();
                self.text.replace_range(self.cursor..end, "");
            }
            KeyCode::Left => self.cursor = self.before(),
            KeyCode::Right => self.cursor = self.after(),
            KeyCode::Home => self.cursor = self.line_start(),
            KeyCode::End => self.cursor = self.line_end(),
            KeyCode::Up if self.multiline => return self.vertical(-1),
            KeyCode::Down if self.multiline => return self.vertical(1),
            _ => return false,
        }
        true
    }

    /// Moves by a text line. False at the first or last line, so a form can
    /// move to another field.
    fn vertical(&mut self, by: i32) -> bool {
        let start = self.line_start();
        let column = self.text[start..self.cursor].chars().count();
        let target = if by < 0 {
            match start.checked_sub(1) {
                Some(end) => self.text[..end].rfind('\n').map_or(0, |at| at + 1),
                None => return false,
            }
        } else {
            let end = self.line_end();
            if end == self.text.len() {
                return false;
            }
            end + 1
        };
        let line = self.text[target..].split('\n').next().unwrap_or_default();
        self.cursor = target
            + line
                .char_indices()
                .nth(column)
                .map_or(line.len(), |(at, _)| at);
        true
    }

    fn delete_word(&mut self) {
        let start = self.word_start();
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    /// The text wrapped at `max` columns and the cursor's place in it.
    pub fn visual(&self, max: usize) -> Visual {
        let mut out = Visual {
            lines: Vec::new(),
            row: 0,
            column: 0,
        };
        let mut offset = 0;
        for line in self.text.split('\n') {
            let ranges = text::wrap_ranges(line, max);
            let last = ranges.len() - 1;
            for (i, range) in ranges.into_iter().enumerate() {
                let (start, end) = (offset + range.start, offset + range.end);
                if self.cursor >= start && (self.cursor < end || (i == last && self.cursor == end))
                {
                    out.row = out.lines.len();
                    out.column = text::width(&self.text[start..self.cursor]);
                }
                out.lines.push(text::raw_line(&line[range]));
            }
            offset += line.len() + 1;
        }
        // At the end of a full line the cursor wraps to the next one.
        if out.column >= max.max(1) {
            out.row += 1;
            out.column = 0;
            out.lines.insert(out.row, String::new());
        }
        out
    }

    /// A one-line window of `max` columns that keeps the cursor visible, and
    /// the cursor's column in it.
    pub fn window(&self, max: usize) -> (String, usize) {
        let max = max.max(1);
        let before = &self.text[..self.cursor];
        let mut start = 0;
        while text::width(&before[start..]) >= max {
            start += before[start..].chars().next().map_or(1, char::len_utf8);
        }
        (
            text::cut(&text::raw_line(&self.text[start..]), max, ""),
            text::width(&before[start..]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(editor: &mut Editor, code: KeyCode, mods: KeyModifiers) -> bool {
        editor.key(KeyEvent::new(code, mods))
    }

    #[test]
    fn edits_by_character_word_and_line() {
        let mut e = Editor::new("héllo world");
        press(&mut e, KeyCode::Left, KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('X'), KeyModifiers::NONE);
        assert_eq!(e.text, "héllo worlXd");
        press(&mut e, KeyCode::Char('w'), KeyModifiers::CONTROL);
        assert_eq!(e.text, "héllo d");
        press(&mut e, KeyCode::Home, KeyModifiers::NONE);
        press(&mut e, KeyCode::Delete, KeyModifiers::NONE);
        press(&mut e, KeyCode::Delete, KeyModifiers::NONE);
        assert_eq!(e.text, "llo d");
        press(&mut e, KeyCode::End, KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert!(e.text.is_empty());
        // AltGr arrives as Ctrl+Alt on Windows and must still type.
        press(
            &mut e,
            KeyCode::Char('@'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        );
        assert_eq!(e.text, "@");
        assert!(!press(&mut e, KeyCode::Enter, KeyModifiers::NONE));
        e.insert("a\nb\n");
        assert_eq!(e.text, "@a b", "one line stays one line");
    }

    #[test]
    fn several_lines_move_vertically_and_wrap() {
        let mut e = Editor::multiline("one\ntwo three");
        e.cursor = 7;
        assert!(press(&mut e, KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(e.cursor, 3);
        assert!(!press(&mut e, KeyCode::Up, KeyModifiers::NONE));
        press(&mut e, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(e.text, "one\n\ntwo three");
        e.cursor = e.text.len();
        let v = e.visual(5);
        assert_eq!(v.lines, ["one", "", "two ", "three", ""]);
        assert_eq!((v.row, v.column), (4, 0));
        e.cursor = 5;
        let v = e.visual(20);
        assert_eq!((v.row, v.column), (2, 0));
    }

    #[test]
    fn a_window_follows_the_cursor() {
        let mut e = Editor::new("東京オフィス");
        let (shown, at) = e.window(6);
        assert_eq!((shown.as_str(), at), ("ィス", 4));
        e.cursor = 0;
        assert_eq!(e.window(6), ("東京オ".to_owned(), 0));
    }
}
