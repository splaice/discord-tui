use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

pub fn fit(s: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if display_width(s) <= width {
        return s.to_string();
    }
    if width == 1 {
        return "…".to_string();
    }
    let mut out = String::new();
    let mut col = 0;
    for ch in s.chars() {
        let w = char_width(ch);
        if col + w > width - 1 {
            break;
        }
        out.push(ch);
        col += w;
    }
    out.push('…');
    out
}

pub fn char_width(ch: char) -> usize {
    UnicodeWidthChar::width(ch).unwrap_or(0)
}

/// Word-wrap a message. Blank lines from `\n\n` are kept.
pub fn wrap_text(input: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    for para in input.split('\n') {
        if para.is_empty() {
            lines.push(String::new());
            continue;
        }
        lines.extend(wrap_paragraph(para, width));
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn wrap_paragraph(para: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut col = 0usize;
    for word in para.split(' ') {
        if word.is_empty() {
            if line.is_empty() {
                continue;
            }
            if col + 1 > width {
                lines.push(std::mem::take(&mut line));
                col = 0;
            } else {
                line.push(' ');
                col += 1;
            }
            continue;
        }
        let word_w = display_width(word);
        if word_w > width {
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                col = 0;
            }
            let parts = hard_wrap(word, width);
            let last = parts.len().saturating_sub(1);
            for (index, part) in parts.into_iter().enumerate() {
                if index == last {
                    col = display_width(&part);
                    line = part;
                } else {
                    lines.push(part);
                }
            }
            continue;
        }
        let needed = if line.is_empty() {
            word_w
        } else {
            col + 1 + word_w
        };
        if needed > width && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
            col = 0;
        }
        if !line.is_empty() {
            line.push(' ');
            col += 1;
        }
        line.push_str(word);
        col += word_w;
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn hard_wrap(input: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut col = 0usize;
    for ch in input.chars() {
        let w = char_width(ch).max(1);
        if col > 0 && col + w > width {
            lines.push(std::mem::take(&mut line));
            col = 0;
        }
        line.push(ch);
        col += w;
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftLine {
    pub text: String,
    pub start: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftWrap {
    pub lines: Vec<DraftLine>,
    pub cursor_line: usize,
    pub cursor_col: usize,
}

/// Soft-wrap a composer draft on `width` columns, and remember where `cursor` sits.
pub fn wrap_draft(text: &str, cursor: usize, width: usize) -> DraftWrap {
    let width = width.max(1);
    let cursor = cursor.min(text.len());
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut col = 0usize;
    let mut line_start = 0usize;
    let mut cursor_line = 0usize;
    let mut cursor_col = 0usize;
    let mut placed = false;

    let push = |lines: &mut Vec<DraftLine>, current: &mut String, line_start: usize| {
        lines.push(DraftLine {
            text: std::mem::take(current),
            start: line_start,
        });
    };

    for (index, ch) in text.char_indices() {
        if ch != '\n' {
            let w = char_width(ch).max(1);
            if !current.is_empty() && col + w > width {
                push(&mut lines, &mut current, line_start);
                line_start = index;
                col = 0;
            }
        }
        if index == cursor && !placed {
            cursor_line = lines.len();
            cursor_col = col;
            placed = true;
        }
        if ch == '\n' {
            push(&mut lines, &mut current, line_start);
            line_start = index + ch.len_utf8();
            col = 0;
            continue;
        }
        let w = char_width(ch).max(1);
        current.push(ch);
        col += w;
    }
    if cursor >= text.len() && !placed {
        cursor_line = lines.len();
        cursor_col = col;
    }
    push(&mut lines, &mut current, line_start);
    if lines.is_empty() {
        lines.push(DraftLine {
            text: String::new(),
            start: 0,
        });
    }
    DraftWrap {
        lines,
        cursor_line,
        cursor_col,
    }
}

/// Byte index at a visual line and column of a wrapped draft.
pub fn draft_byte(text: &str, width: usize, line: usize, col: usize) -> usize {
    let wrap = wrap_draft(text, text.len(), width);
    let Some(piece) = wrap.lines.get(line) else {
        return text.len();
    };
    let mut byte = piece.start;
    let mut seen = 0usize;
    for ch in piece.text.chars() {
        let w = char_width(ch).max(1);
        if seen + w > col {
            break;
        }
        byte += ch.len_utf8();
        seen += w;
    }
    byte
}

pub fn window_start(len: usize, height: usize, cursor: usize) -> usize {
    if height == 0 || len <= height {
        return 0;
    }
    let mut start = 0;
    if cursor + 1 > height {
        start = cursor + 1 - height;
    }
    start.min(len - height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_on_words() {
        let lines = wrap_text("one two three", 7);
        assert_eq!(lines, vec!["one two".to_string(), "three".to_string()]);
    }

    #[test]
    fn draft_cursor_tracks_a_newline() {
        let wrap = wrap_draft("ab\ncd", 3, 8);
        assert_eq!(wrap.cursor_line, 1);
        assert_eq!(wrap.cursor_col, 0);
        assert_eq!(wrap.lines[0].text, "ab");
        assert_eq!(wrap.lines[1].text, "cd");
    }

    #[test]
    fn draft_soft_wraps_and_keeps_the_cursor() {
        let wrap = wrap_draft("hello", 5, 3);
        assert_eq!(wrap.lines.len(), 2);
        assert_eq!(wrap.lines[0].text, "hel");
        assert_eq!(wrap.lines[1].text, "lo");
        assert_eq!(wrap.cursor_line, 1);
        assert_eq!(wrap.cursor_col, 2);
        assert_eq!(draft_byte("hello", 3, 0, 1), 1);
    }
}
