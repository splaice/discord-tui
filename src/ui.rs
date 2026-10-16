use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Focus, Link};
use crate::model::{self, Kind, LineKind, Marker, NavRow};
use crate::screen::Screen;
use crate::text::{display_width, fit, window_start, wrap_draft};
use crate::theme::Theme;

const HEADER_H: u16 = 3;
const COMPOSER_H: u16 = 9;

pub fn draw(frame: &mut Frame, app: &App) -> Screen {
    let theme = Theme::grok_night();
    let area = frame.area();
    let buf = frame.buffer_mut();
    fill(buf, area, theme.bg, theme.text);
    if area.width < 72 || area.height < 18 {
        put(
            buf,
            area.x + 1,
            area.y + 1,
            area.right(),
            "Terminal too small",
            theme.text(),
        );
        put(
            buf,
            area.x + 1,
            area.y + 3,
            area.right(),
            "q quit",
            theme.dim(),
        );
        return Screen::default();
    }

    let [main, status] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);
    let left_w = ((main.width as u32 * 30) / 100).clamp(26, 36) as u16;
    let left_w = left_w.min(main.width.saturating_sub(36));
    let [left, divider, right] = Layout::horizontal([
        Constraint::Length(left_w),
        Constraint::Length(1),
        Constraint::Min(20),
    ])
    .areas(main);

    for y in divider.y..divider.bottom() {
        buf[(divider.x, y)]
            .set_symbol("│")
            .set_style(theme.fg(theme.border));
    }

    let [left_head, left_body] =
        Layout::vertical([Constraint::Length(HEADER_H), Constraint::Min(1)]).areas(left);
    let [right_head, right_body] =
        Layout::vertical([Constraint::Length(HEADER_H), Constraint::Min(1)]).areas(right);
    let rule_y = left_head.bottom().saturating_sub(1);
    hline(buf, left.x, rule_y, left.width, theme.border);
    hline(buf, right.x, rule_y, right.width, theme.border);
    buf[(divider.x, rule_y)]
        .set_symbol("┼")
        .set_style(theme.fg(theme.border));

    draw_left_header(buf, left_head, app, theme);
    draw_right_header(buf, right_head, app, theme);

    let mut screen = Screen::default();
    let filter = Rect {
        x: left_body.x,
        y: left_body.y,
        width: left_body.width,
        height: 1,
    };
    let list = Rect {
        x: left_body.x,
        y: left_body.y + 1,
        width: left_body.width,
        height: left_body.height.saturating_sub(1),
    };
    screen.filter = Some(filter);
    draw_filter(buf, filter, app, theme);
    screen.nav_offset = draw_nav(buf, list, app, theme, &mut screen);

    let [history, composer] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(COMPOSER_H.min(right_body.height.saturating_sub(1))),
    ])
    .areas(right_body);
    draw_transcript(buf, history, app, theme, &mut screen);
    let cursor = draw_composer(buf, composer, divider.x, app, theme, &mut screen);

    draw_status(buf, status, app, theme);
    if app.help {
        draw_help(frame, theme);
    } else if app.focus == Focus::Filter {
        place_filter_cursor(frame, app, filter);
    } else if app.focus == Focus::Composer
        && let Some((x, y)) = cursor
    {
        frame.set_cursor_position((x, y));
    }
    screen
}

fn draw_left_header(buf: &mut ratatui::buffer::Buffer, area: Rect, app: &App, theme: Theme) {
    let max_x = area.right();
    let title = if matches!(app.focus, Focus::Nav | Focus::Filter) {
        theme.bold_accent()
    } else {
        theme.faint()
    };
    put(buf, area.x + 1, area.y, max_x, "chats", title);
    let count = app.session.chat_count().to_string();
    let count_x = max_x.saturating_sub(1 + display_width(&count) as u16);
    if count_x > area.x + 8 {
        put(buf, count_x, area.y, max_x, &count, theme.faint());
    }
    let account_style = if matches!(app.account.as_str(), "not signed in" | "connecting") {
        theme.dim()
    } else {
        theme.ok().add_modifier(ratatui::style::Modifier::BOLD)
    };
    put(
        buf,
        area.x + 1,
        area.y + 1,
        max_x.saturating_sub(1),
        &app.account,
        account_style,
    );
}

fn draw_right_header(buf: &mut ratatui::buffer::Buffer, area: Rect, app: &App, theme: Theme) {
    let max_x = area.right().saturating_sub(1);
    let title = if matches!(app.focus, Focus::Transcript | Focus::Composer) {
        theme.bold_accent()
    } else {
        theme.faint()
    };
    put(buf, area.x + 1, area.y, max_x, "messages", title);
    let Some(convo) = app.open_convo() else {
        put(buf, area.x + 1, area.y + 1, max_x, "No chat", theme.dim());
        return;
    };
    let name_x = if convo.kind == Kind::Channel {
        put(buf, area.x + 1, area.y + 1, max_x, "#", theme.faint());
        area.x + 3
    } else {
        area.x + 1
    };
    let server = server_name(app, convo.id);
    let server_w = server.map(display_width).unwrap_or(0) as u16;
    let name_end = if server_w == 0 {
        max_x
    } else {
        max_x.saturating_sub(server_w + 2)
    };
    put(
        buf,
        name_x,
        area.y + 1,
        name_end,
        &convo.title,
        theme.bold(),
    );
    if let Some(server) = server
        && name_end + 2 < max_x
    {
        put(
            buf,
            max_x.saturating_sub(server_w),
            area.y + 1,
            area.right(),
            server,
            theme.faint(),
        );
    }
}

fn server_name(app: &App, id: model::ConvId) -> Option<&str> {
    app.session
        .servers
        .iter()
        .find(|server| server.channels.contains(&id))
        .map(|server| server.name.as_str())
}

fn draw_filter(buf: &mut ratatui::buffer::Buffer, area: Rect, app: &App, theme: Theme) {
    let max_x = area.right().saturating_sub(1);
    let slash = if app.focus == Focus::Filter {
        theme.bold_accent()
    } else {
        theme.faint()
    };
    put(buf, area.x + 1, area.y, max_x, "/", slash);
    if app.filter.is_empty() && app.focus != Focus::Filter {
        put(buf, area.x + 3, area.y, max_x, "filter", theme.faint());
    } else {
        put(buf, area.x + 3, area.y, max_x, &app.filter, theme.text());
    }
}

fn draw_nav(
    buf: &mut ratatui::buffer::Buffer,
    area: Rect,
    app: &App,
    theme: Theme,
    screen: &mut Screen,
) -> usize {
    screen.nav_area = Some(area);
    screen.nav_height = area.height as usize;
    let rows = app.rows();
    let height = area.height as usize;
    let offset = kept_offset(app.nav_offset, app.nav_cursor, rows.len(), height);
    if rows.is_empty() {
        let label = if !app.filter.trim().is_empty() {
            "No matches"
        } else {
            match app.link {
                Link::SignedOut => "dt login",
                Link::Connecting | Link::Reconnecting => "Connecting",
                _ => "No chats",
            }
        };
        put(
            buf,
            area.x + 1,
            area.y,
            area.right().saturating_sub(1),
            label,
            theme.dim(),
        );
        return 0;
    }
    let nav_focus = matches!(app.focus, Focus::Nav | Focus::Filter);
    for (index, row) in rows.iter().enumerate().skip(offset).take(height) {
        let y = area.y + (index - offset) as u16;
        let rect = Rect {
            x: area.x,
            y,
            width: area.width,
            height: 1,
        };
        let selected = index == app.nav_cursor;
        if selected {
            let bg = if nav_focus {
                theme.bg_selected
            } else {
                theme.bg_field
            };
            fill(buf, rect, bg, theme.text);
        }
        draw_nav_row(buf, rect, row, selected, nav_focus, theme);
        screen.nav_hits.push((rect, index));
    }
    offset
}

fn draw_nav_row(
    buf: &mut ratatui::buffer::Buffer,
    area: Rect,
    row: &NavRow,
    selected: bool,
    nav_focus: bool,
    theme: Theme,
) {
    let max_x = area.right().saturating_sub(1);
    let mut x = area.x + 1 + u16::from(row.depth) * 2;
    if row.marker == Marker::None {
        if row.depth > 0 {
            x = x.saturating_add(2);
        }
    } else {
        let glyph = match row.marker {
            Marker::Open => "▾",
            Marker::Closed => "▸",
            Marker::Channel => "#",
            Marker::None => "",
        };
        let style = if selected && nav_focus {
            theme.accent()
        } else if row.marker == Marker::Channel {
            theme.faint()
        } else {
            theme.dim()
        };
        put(buf, x, area.y, max_x, glyph, style);
        x = x.saturating_add(2);
    }
    let unread = if row.unread == 0 {
        String::new()
    } else if row.unread > 99 {
        "99+".to_string()
    } else {
        row.unread.to_string()
    };
    let unread_w = display_width(&unread) as u16;
    let name_end = if unread_w == 0 {
        max_x
    } else {
        max_x.saturating_sub(unread_w + 1)
    };
    let name_style = if selected && nav_focus {
        theme.bold_accent()
    } else if selected {
        theme.bold()
    } else if row.depth == 0 {
        theme.faint()
    } else {
        theme.text()
    };
    if x < name_end {
        put(buf, x, area.y, name_end, &row.label, name_style);
    }
    if unread_w > 0 && name_end < max_x {
        let style = if selected && nav_focus {
            theme.bold_accent()
        } else {
            theme.accent()
        };
        put(
            buf,
            max_x.saturating_sub(unread_w),
            area.y,
            area.right(),
            &unread,
            style,
        );
    }
}

fn draw_transcript(
    buf: &mut ratatui::buffer::Buffer,
    area: Rect,
    app: &App,
    theme: Theme,
    screen: &mut Screen,
) {
    screen.transcript = Some(area);
    let width = area.width.saturating_sub(2) as usize;
    screen.transcript_width = width.max(1);
    let lines = app.session.transcript(app.open, width.max(1));
    let height = area.height as usize;
    // Leave one quiet row above the composer while the tail is followed.
    let pad = if app.follow { 1.min(height) } else { 0 };
    let draw_height = height - pad;
    screen.transcript_height = draw_height;
    screen.transcript_lines = lines.len();
    if lines.is_empty() {
        let note = app.empty_note();
        if note.len() <= 1 {
            let text = note.first().map(String::as_str).unwrap_or("No messages");
            put(
                buf,
                area.x + 1,
                area.bottom().saturating_sub(1 + pad as u16),
                area.right().saturating_sub(1),
                text,
                theme.dim(),
            );
        } else {
            for (index, line) in note.iter().enumerate() {
                let y = area.y + 1 + index as u16;
                if y + 1 >= area.bottom() {
                    break;
                }
                let style = if index == 0 {
                    theme.text()
                } else {
                    theme.dim()
                };
                put(
                    buf,
                    area.x + 1,
                    y,
                    area.right().saturating_sub(1),
                    line,
                    style,
                );
            }
        }
        return;
    }
    let start = model::line_window(lines.len(), draw_height, app.scroll, app.follow);
    let y0 = if app.follow && lines.len() < draw_height {
        area.y + (draw_height - lines.len()) as u16
    } else {
        area.y
    };
    let max_x = area.right().saturating_sub(1);
    for (drawn, line) in lines.iter().skip(start).take(draw_height).enumerate() {
        let y = y0 + drawn as u16;
        match line.kind {
            LineKind::Blank => {}
            LineKind::Meta { mine } => {
                let name_style = if mine {
                    theme.bold_accent()
                } else {
                    theme.bold()
                };
                let time_w = display_width(&line.time) as u16;
                let time_x = max_x.saturating_sub(time_w);
                let name_end = time_x.saturating_sub(1);
                put(buf, area.x + 1, y, name_end, &line.text, name_style);
                if time_w > 0 && time_x > area.x + 8 {
                    put(buf, time_x, y, max_x, &line.time, theme.faint());
                }
            }
            LineKind::Body => {
                put(buf, area.x + 1, y, max_x, &line.text, theme.text());
            }
        }
    }
    let marker = if start + draw_height < lines.len() {
        Some("↓")
    } else if start > 0 {
        Some("↑")
    } else {
        None
    };
    if let Some(marker) = marker {
        put(
            buf,
            area.right().saturating_sub(1),
            area.bottom().saturating_sub(1),
            area.right(),
            marker,
            theme.faint(),
        );
    }
}

fn draw_composer(
    buf: &mut ratatui::buffer::Buffer,
    area: Rect,
    divider_x: u16,
    app: &App,
    theme: Theme,
    screen: &mut Screen,
) -> Option<(u16, u16)> {
    if area.width < 12 || area.height < 7 {
        return None;
    }
    let quiet = theme.fg(theme.border);
    let right = area.right().saturating_sub(1);
    let bottom = area.bottom().saturating_sub(1);
    set(buf, divider_x, area.y, "├", quiet);
    for x in area.x..right {
        set(buf, x, area.y, "─", quiet);
    }
    set(buf, right, area.y, "┐", quiet);
    for y in area.y + 1..bottom {
        set(buf, divider_x, y, "│", quiet);
        set(buf, right, y, "│", quiet);
    }
    set(buf, divider_x, bottom, "└", quiet);
    for x in area.x..right {
        set(buf, x, bottom, "─", quiet);
    }
    set(buf, right, bottom, "┘", quiet);

    let prompt = Rect {
        x: area.x + 1,
        y: area.y + 2,
        width: area.width.saturating_sub(3),
        height: area.height.saturating_sub(4),
    };
    if prompt.width < 8 || prompt.height < 4 {
        return None;
    }
    let focused = app.focus == Focus::Composer;
    let accent = if focused {
        theme.accent()
    } else {
        theme.fg(theme.border)
    };
    draw_round(buf, prompt, accent);
    let interior = Rect {
        x: prompt.x + 1,
        y: prompt.y + 1,
        width: prompt.width.saturating_sub(2),
        height: prompt.height.saturating_sub(2),
    };
    fill(buf, interior, theme.bg, theme.text);
    screen.composer_box = Some(prompt);

    let footer_y = interior.bottom().saturating_sub(1);
    let text_rows = interior.height.saturating_sub(1);
    let chevron_x = interior.x + 1;
    let text_x = chevron_x + 2;
    let text_right = interior.right().saturating_sub(1);
    let text_w = text_right.saturating_sub(text_x) as usize;
    if text_w == 0 || text_rows == 0 {
        return None;
    }
    let text_area = Rect {
        x: text_x,
        y: interior.y,
        width: text_w as u16,
        height: text_rows,
    };
    screen.composer = Some(text_area);
    screen.composer_width = text_w;

    let draft = app.draft();
    let wrap = wrap_draft(&draft.text, draft.cursor, text_w);
    let first = window_start(wrap.lines.len(), text_rows as usize, wrap.cursor_line);
    screen.composer_first = first;
    let chevron = if focused {
        theme.bold_accent()
    } else {
        theme.faint()
    };
    if first == 0 {
        set(buf, chevron_x, interior.y, ">", chevron);
    }
    if draft.text.is_empty()
        && let Some(convo) = app.open_convo()
    {
        put(
            buf,
            text_x,
            interior.y,
            text_area.right(),
            &model::placeholder(convo),
            theme.faint(),
        );
    }
    for (index, line) in wrap
        .lines
        .iter()
        .enumerate()
        .skip(first)
        .take(text_rows as usize)
    {
        let y = interior.y + (index - first) as u16;
        put(buf, text_x, y, text_area.right(), &line.text, theme.text());
    }
    draw_prompt_footer(buf, interior, footer_y, app, theme);

    let row = wrap.cursor_line.saturating_sub(first);
    if row >= text_rows as usize {
        return None;
    }
    let x = text_x
        .saturating_add(wrap.cursor_col as u16)
        .min(text_area.right());
    let y = interior.y + row as u16;
    (x < prompt.right()).then_some((x, y))
}

fn draw_prompt_footer(
    buf: &mut ratatui::buffer::Buffer,
    interior: Rect,
    y: u16,
    app: &App,
    theme: Theme,
) {
    let label = prompt_label(app);
    let label_w = display_width(&label) as u16;
    let label_x = interior.right().saturating_sub(1 + label_w).max(interior.x);
    put(buf, label_x, y, interior.right(), &label, theme.faint());
}

fn prompt_label(app: &App) -> String {
    match app.open_convo() {
        Some(convo) if convo.kind == Kind::Channel => format!("#{} · enter send", convo.title),
        Some(convo) => format!("{} · enter send", convo.title),
        None => "enter send".to_string(),
    }
}

fn draw_round(buf: &mut ratatui::buffer::Buffer, area: Rect, style: Style) {
    if area.width < 2 || area.height < 2 {
        return;
    }
    let right = area.right().saturating_sub(1);
    let bottom = area.bottom().saturating_sub(1);
    set(buf, area.x, area.y, "╭", style);
    for x in area.x + 1..right {
        set(buf, x, area.y, "─", style);
    }
    set(buf, right, area.y, "╮", style);
    for y in area.y + 1..bottom {
        set(buf, area.x, y, "│", style);
        set(buf, right, y, "│", style);
    }
    set(buf, area.x, bottom, "╰", style);
    for x in area.x + 1..right {
        set(buf, x, bottom, "─", style);
    }
    set(buf, right, bottom, "╯", style);
}

fn draw_status(buf: &mut ratatui::buffer::Buffer, area: Rect, app: &App, theme: Theme) {
    fill(buf, area, theme.bg_raised, theme.dim);
    let right = if let Some(toast) = &app.toast {
        let style = if toast.error {
            theme.fg(theme.danger)
        } else {
            theme.accent()
        };
        (toast.text.clone(), style)
    } else {
        (app.status_count(), theme.faint())
    };
    let right_w = display_width(&right.0) as u16;
    let right_x = area.right().saturating_sub(1 + right_w);
    draw_hints(
        buf,
        area.x + 1,
        area.y,
        right_x.saturating_sub(2),
        app,
        theme,
    );
    if right_x > area.x {
        put(buf, right_x, area.y, area.right(), &right.0, right.1);
    }
}

fn draw_hints(
    buf: &mut ratatui::buffer::Buffer,
    mut x: u16,
    y: u16,
    max_x: u16,
    app: &App,
    theme: Theme,
) {
    let hints: &[(&str, bool)] = if app.help {
        &[
            ("esc", true),
            (" close", false),
            ("  ", false),
            ("q", true),
            (" quit", false),
        ]
    } else {
        match app.focus {
            Focus::Filter => &[
                ("type", false),
                ("  ", false),
                ("↑↓", true),
                (" move", false),
                ("  ", false),
                ("enter", true),
                (" done", false),
                ("  ", false),
                ("esc", true),
                (" clear", false),
            ],
            Focus::Nav => &[
                ("↑↓", true),
                (" move", false),
                ("  ", false),
                ("space", true),
                (" fold", false),
                ("  ", false),
                ("tab", true),
                (" read", false),
                ("  ", false),
                ("/", true),
                (" filter", false),
                ("  ", false),
                ("?", true),
                (" help", false),
            ],
            Focus::Transcript => &[
                ("↑↓", true),
                (" scroll", false),
                ("  ", false),
                ("g G", true),
                (" ends", false),
                ("  ", false),
                ("tab", true),
                (" write", false),
                ("  ", false),
                ("y", true),
                (" copy", false),
                ("  ", false),
                ("esc", true),
                (" chats", false),
            ],
            Focus::Composer => &[
                ("enter", true),
                (" send", false),
                ("  ", false),
                ("ctrl-j", true),
                (" newline", false),
                ("  ", false),
                ("esc ←", true),
                (" back", false),
            ],
        }
    };
    for (text, key) in hints {
        let width = UnicodeWidthStr::width(*text) as u16;
        if x.saturating_add(width) > max_x {
            break;
        }
        let style = if *key { theme.accent() } else { theme.dim() };
        put(buf, x, y, max_x.saturating_add(1), text, style);
        x = x.saturating_add(width);
    }
}

fn draw_help(frame: &mut Frame, theme: Theme) {
    let lines = [
        "↑↓ j k        move",
        "ctrl-u d      half page",
        "g G           top and bottom",
        "tab           chats, messages, composer",
        "h l  ← →      fold, or switch pane",
        "space         fold a section or a server",
        "enter         open a chat, or send",
        "/             filter chats",
        "ctrl-j        newline in the composer",
        "esc  ←        leave the composer",
        "y             copy the latest message",
        "esc           back, or clear a filter",
        "?             help",
        "q             quit",
        "",
        "Direct messages sit above servers.",
        "A server opens onto its text channels.",
        "The composer stays put while history scrolls.",
    ];
    let width = 58.min(frame.area().width.saturating_sub(4));
    let height = (lines.len() as u16 + 2).min(frame.area().height.saturating_sub(2));
    let area = Rect {
        x: frame.area().x + (frame.area().width.saturating_sub(width)) / 2,
        y: frame.area().y + (frame.area().height.saturating_sub(height)) / 2,
        width,
        height,
    };
    let block = Block::default()
        .title(Line::from(" keys "))
        .borders(Borders::ALL)
        .border_style(theme.accent())
        .style(Style::new().bg(theme.bg_raised).fg(theme.text));
    frame.render_widget(Clear, area);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let text: Vec<Line> = lines.iter().map(|line| Line::from(*line)).collect();
    frame.render_widget(Paragraph::new(text).style(theme.text()), inner);
}

fn place_filter_cursor(frame: &mut Frame, app: &App, filter: Rect) {
    let x = filter.x + 3 + display_width(&app.filter) as u16;
    let max_x = filter.right().saturating_sub(1);
    if x < max_x {
        frame.set_cursor_position((x, filter.y));
    }
}

fn kept_offset(offset: usize, cursor: usize, len: usize, height: usize) -> usize {
    if height == 0 || len == 0 {
        return 0;
    }
    let offset = offset.min(len.saturating_sub(height));
    if cursor < offset {
        cursor
    } else if cursor >= offset + height {
        cursor + 1 - height
    } else {
        offset
    }
}

fn hline(
    buf: &mut ratatui::buffer::Buffer,
    x: u16,
    y: u16,
    width: u16,
    color: ratatui::style::Color,
) {
    let style = Style::new().fg(color);
    for col in x..x.saturating_add(width) {
        buf[(col, y)].set_symbol("─").set_style(style);
    }
}

fn fill(
    buf: &mut ratatui::buffer::Buffer,
    area: Rect,
    bg: ratatui::style::Color,
    fg: ratatui::style::Color,
) {
    let style = Style::new().bg(bg).fg(fg);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            buf[(x, y)].set_symbol(" ").set_style(style);
        }
    }
}

fn set(buf: &mut ratatui::buffer::Buffer, x: u16, y: u16, symbol: &str, style: Style) {
    if x < buf.area.width && y < buf.area.height {
        buf[(x, y)].set_symbol(symbol).set_style(style);
    }
}

fn put(
    buf: &mut ratatui::buffer::Buffer,
    x: u16,
    y: u16,
    max_x: u16,
    text: &str,
    style: Style,
) -> Rect {
    if y >= buf.area.height || x >= max_x || x >= buf.area.width {
        return Rect {
            x,
            y,
            width: 0,
            height: 1,
        };
    }
    let max_x = max_x.min(buf.area.width);
    let fitted = fit(text, (max_x - x) as usize);
    let width = UnicodeWidthStr::width(fitted.as_str()) as u16;
    if width > 0 {
        buf.set_string(x, y, &fitted, style);
    }
    Rect {
        x,
        y,
        width,
        height: 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    fn render(app: &App, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                draw(frame, app);
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn plain(buf: &ratatui::buffer::Buffer) -> String {
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let symbol = buf[(x, y)].symbol();
                if symbol.is_empty() {
                    out.push(' ');
                } else {
                    out.push_str(symbol);
                }
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn layout_shows_chats_history_and_composer() {
        let app = App::sample();
        let text = plain(&render(&app, 110, 36));
        assert!(text.contains("chats"), "{text}");
        assert!(text.contains("Direct messages"), "{text}");
        assert!(text.contains("Mara"), "{text}");
        assert!(text.contains("forge"), "{text}");
        assert!(text.contains("releases"), "{text}");
        assert!(!text.contains("specs"), "{text}");
        assert!(text.contains("messages"), "{text}");
        assert!(text.contains("Message Mara"), "{text}");
        assert!(text.contains("Mara · enter send"), "{text}");
        assert!(text.contains('>'), "{text}");
        assert!(text.contains("The accent on the focused row"), "{text}");
        let rows: Vec<&str> = text.lines().collect();
        let border = rows.iter().position(|row| row.contains('├')).unwrap();
        let tail = rows
            .iter()
            .position(|row| row.contains("can stay quiet"))
            .unwrap();
        assert_eq!(
            border - tail,
            2,
            "latest message should sit just above the composer"
        );
    }

    #[test]
    fn composer_border_uses_the_accent_when_focused() {
        let mut app = App::sample();
        app.focus = Focus::Composer;
        let buf = render(&app, 110, 36);
        let accent = Color::Rgb(226, 120, 166);
        let mut found = false;
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                if buf[(x, y)].symbol() == "╭" && buf[(x, y)].style().fg == Some(accent) {
                    found = true;
                }
            }
        }
        assert!(found, "focused prompt should use the accent");
    }

    #[test]
    fn signed_out_explains_login() {
        let app = App::signed_out();
        let text = plain(&render(&app, 110, 36));
        assert!(text.contains("not signed in"), "{text}");
        assert!(text.contains("dt login"), "{text}");
        assert!(text.contains("Not signed in"), "{text}");
        assert!(!text.contains("Mara"), "{text}");
    }
}
