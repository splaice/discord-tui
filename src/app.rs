use std::collections::HashSet;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};

use crate::discord::{self, Cmd, Ev};
use crate::model::{self, ConvId, Convo, Draft, Kind, Message, Session, Target};
use crate::screen::{self, Screen};
use crate::text::{draft_byte, wrap_draft};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Nav,
    Transcript,
    Composer,
    Filter,
}

#[derive(Clone, Debug)]
pub enum Effect {
    None,
    Quit,
    Copy { text: String, toast: String },
}

#[derive(Clone, Debug)]
pub struct Toast {
    pub text: String,
    pub error: bool,
    pub until: Instant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Link {
    SignedOut,
    Connecting,
    Ready,
    Reconnecting,
    Failed(String),
}

pub struct App {
    pub session: Session,
    pub focus: Focus,
    pub help: bool,
    pub filter: String,
    pub nav_cursor: usize,
    pub nav_offset: usize,
    pub open: ConvId,
    pub follow: bool,
    pub scroll: usize,
    pub toast: Option<Toast>,
    pub screen: Screen,
    pub account: String,
    pub link: Link,
    pub live: bool,
    loading: HashSet<ConvId>,
    loaded: HashSet<ConvId>,
    exhausted: HashSet<ConvId>,
    outbox: Vec<Cmd>,
}

impl App {
    #[cfg(test)]
    pub fn sample() -> Self {
        let session = model::sample();
        let open = session.dms[0];
        let mut app = Self {
            session,
            focus: Focus::Nav,
            help: false,
            filter: String::new(),
            nav_cursor: 0,
            nav_offset: 0,
            open,
            follow: true,
            scroll: 0,
            toast: None,
            screen: Screen::default(),
            account: "you".into(),
            link: Link::Ready,
            live: false,
            loading: HashSet::new(),
            loaded: HashSet::new(),
            exhausted: HashSet::new(),
            outbox: Vec::new(),
        };
        app.nav_cursor = app
            .rows()
            .iter()
            .position(|row| row.chat() == Some(open))
            .unwrap_or(0);
        app.session.mark_read(open);
        app
    }

    pub fn signed_out() -> Self {
        Self {
            session: model::empty(),
            focus: Focus::Nav,
            help: false,
            filter: String::new(),
            nav_cursor: 0,
            nav_offset: 0,
            open: ConvId(0),
            follow: true,
            scroll: 0,
            toast: None,
            screen: Screen::default(),
            account: "not signed in".into(),
            link: Link::SignedOut,
            live: true,
            loading: HashSet::new(),
            loaded: HashSet::new(),
            exhausted: HashSet::new(),
            outbox: Vec::new(),
        }
    }

    pub fn connecting() -> Self {
        let mut app = Self::signed_out();
        app.link = Link::Connecting;
        app.account = "connecting".into();
        app
    }

    pub fn drain_cmds(&mut self) -> Vec<Cmd> {
        std::mem::take(&mut self.outbox)
    }

    pub fn empty_note(&self) -> Vec<String> {
        if self.loading.contains(&self.open) && self.session.messages(self.open).is_empty() {
            return vec!["Loading".into()];
        }
        match &self.link {
            Link::SignedOut => vec![
                "Not signed in".into(),
                String::new(),
                "Run dt login and paste an account token.".into(),
                "README 'Sign in' shows how to copy it from your Discord client.".into(),
            ],
            Link::Connecting => vec!["Connecting".into()],
            Link::Reconnecting if !self.session.visible(self.open) => vec!["Reconnecting".into()],
            Link::Failed(message) if self.session.chat_count() == 0 => {
                vec!["Could not connect".into(), String::new(), message.clone()]
            }
            Link::Ready | Link::Reconnecting | Link::Failed(_)
                if self.live && self.session.chat_count() == 0 =>
            {
                vec![
                    "No chats yet".into(),
                    String::new(),
                    "Your servers and direct messages will appear here.".into(),
                ]
            }
            _ => vec!["No messages".into()],
        }
    }

    pub fn on_event(&mut self, event: Ev) {
        match event {
            Ev::Connecting => self.link = Link::Connecting,
            Ev::Reconnecting => self.link = Link::Reconnecting,
            Ev::Ready { name } => {
                self.account = name;
                self.link = Link::Ready;
            }
            Ev::Failed(message) => self.link = Link::Failed(message),
            Ev::Dms(convos) => {
                let previous = self.cursor_target();
                self.install_dms(convos);
                self.after_structure(previous);
            }
            Ev::Guild(guild) => {
                let previous = self.cursor_target();
                self.install_guild(guild.id, guild.name, guild.channels);
                self.after_structure(previous);
            }
            Ev::GuildName { id, name } => {
                let previous = self.cursor_target();
                if let Some(server) = self.session.server_mut(id) {
                    server.name = name;
                }
                self.session.servers.sort_by(|left, right| {
                    left.name
                        .to_lowercase()
                        .cmp(&right.name.to_lowercase())
                        .then(left.id.cmp(&right.id))
                });
                self.after_structure(previous);
            }
            Ev::GuildRemoved(id) => {
                let previous = self.cursor_target();
                self.session.servers.retain(|server| server.id != id);
                self.after_structure(previous);
            }
            Ev::Channel { id, guild, convo } => {
                let previous = self.cursor_target();
                self.install_channel(id, guild, convo);
                self.after_structure(previous);
            }
            Ev::History {
                channel,
                older,
                messages,
                more,
            } => self.install_history(channel, older, messages, more),
            Ev::HistoryFailed { channel, error } => {
                self.loading.remove(&ConvId(channel));
                self.toast(error, true);
            }
            Ev::Created {
                channel,
                message,
                dm,
            } => self.install_created(channel, message, dm),
            Ev::Edited {
                channel,
                message,
                body,
            } => {
                let id = ConvId(channel);
                if let Some(list) = self.session.messages.get_mut(&id)
                    && let Some(item) = list.iter_mut().find(|item| item.id == message)
                {
                    item.body = body;
                }
            }
            Ev::Deleted { channel, message } => {
                if let Some(list) = self.session.messages.get_mut(&ConvId(channel)) {
                    list.retain(|item| item.id != message);
                }
            }
            Ev::SendFailed {
                channel,
                nonce,
                error,
            } => {
                let id = ConvId(channel);
                if let Some(message) = self.session.take_pending(id, &nonce) {
                    self.session.restore_draft(id, &message.body);
                }
                self.toast(error, true);
            }
            Ev::Notice(message) => self.toast(message, true),
        }
    }

    pub fn rows(&self) -> Vec<model::NavRow> {
        self.session.rows(&self.filter)
    }

    pub fn open_convo(&self) -> Option<&model::Convo> {
        self.session.convo(self.open)
    }

    pub fn adopt(&mut self, screen: Screen) {
        self.nav_offset = screen.nav_offset;
        self.screen = screen;
    }

    pub fn poll_timeout(&self) -> Duration {
        let idle = Duration::from_millis(if self.live { 50 } else { 200 });
        match &self.toast {
            Some(toast) => {
                let now = Instant::now();
                if toast.until <= now {
                    Duration::from_millis(1)
                } else {
                    (toast.until - now).min(idle)
                }
            }
            None => idle,
        }
    }

    pub fn tick(&mut self) {
        if self
            .toast
            .as_ref()
            .is_some_and(|toast| toast.until <= Instant::now())
        {
            self.toast = None;
        }
    }

    pub fn toast(&mut self, text: impl Into<String>, error: bool) {
        self.toast = Some(Toast {
            text: text.into(),
            error,
            until: Instant::now() + Duration::from_millis(if error { 3200 } else { 1600 }),
        });
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Effect {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Effect::Quit;
        }
        if self.help {
            return match key.code {
                KeyCode::Esc | KeyCode::Char('?') => {
                    self.help = false;
                    Effect::None
                }
                KeyCode::Char('q') => Effect::Quit,
                _ => Effect::None,
            };
        }
        if self.focus == Focus::Filter {
            return self.filter_key(key);
        }
        if self.focus == Focus::Composer {
            return self.composer_key(key);
        }
        self.pane_key(key)
    }

    pub fn on_mouse(&mut self, mouse: MouseEvent) -> Effect {
        if self.help {
            if matches!(mouse.kind, MouseEventKind::Down(_)) {
                self.help = false;
            }
            return Effect::None;
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => self.wheel(mouse.column, mouse.row, -3),
            MouseEventKind::ScrollDown => self.wheel(mouse.column, mouse.row, 3),
            MouseEventKind::Down(_) => self.click(mouse.column, mouse.row),
            _ => Effect::None,
        }
    }

    pub fn on_paste(&mut self, text: String) -> Effect {
        match self.focus {
            Focus::Composer => {
                self.session.draft_mut(self.open).insert(&text);
            }
            Focus::Filter => {
                for ch in text.chars().filter(|ch| !ch.is_control()) {
                    if self.filter.chars().count() >= 64 {
                        break;
                    }
                    self.filter.push(ch);
                }
                self.realign();
            }
            Focus::Nav | Focus::Transcript => {}
        }
        Effect::None
    }

    fn filter_key(&mut self, key: KeyEvent) -> Effect {
        match key.code {
            KeyCode::Esc => {
                self.filter.clear();
                self.focus = Focus::Nav;
                self.realign();
                Effect::None
            }
            KeyCode::Enter => {
                self.focus = Focus::Nav;
                Effect::None
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.realign();
                Effect::None
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.filter.chars().count() < 64 && !ch.is_control() {
                    self.filter.push(ch);
                    self.realign();
                }
                Effect::None
            }
            KeyCode::Up => self.move_nav(-1),
            KeyCode::Down => self.move_nav(1),
            KeyCode::Home => self.jump_nav(true),
            KeyCode::End => self.jump_nav(false),
            KeyCode::Tab => {
                self.focus = Focus::Nav;
                self.cycle(1)
            }
            KeyCode::BackTab => {
                self.focus = Focus::Nav;
                self.cycle(-1)
            }
            _ => self.page_key(key, true),
        }
    }

    fn composer_key(&mut self, key: KeyEvent) -> Effect {
        if !self.session.visible(self.open) {
            return match key.code {
                KeyCode::Esc | KeyCode::Left => {
                    self.focus = Focus::Transcript;
                    Effect::None
                }
                KeyCode::Tab => self.cycle(1),
                KeyCode::BackTab => self.cycle(-1),
                _ => Effect::None,
            };
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        if key.code == KeyCode::Enter && !ctrl && !shift && !alt {
            return self.send();
        }
        if key.code == KeyCode::Enter || (ctrl && matches!(key.code, KeyCode::Char('j'))) {
            self.session.draft_mut(self.open).insert("\n");
            return Effect::None;
        }
        if ctrl && matches!(key.code, KeyCode::Char('u')) {
            self.session.draft_mut(self.open).clear();
            return Effect::None;
        }
        if ctrl && matches!(key.code, KeyCode::Char('w')) {
            self.session.draft_mut(self.open).kill_word();
            return Effect::None;
        }
        if ctrl && matches!(key.code, KeyCode::Char('a')) {
            self.session.draft_mut(self.open).cursor = 0;
            return Effect::None;
        }
        if ctrl && matches!(key.code, KeyCode::Char('e')) {
            let draft = self.session.draft_mut(self.open);
            draft.cursor = draft.text.len();
            return Effect::None;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Left => {
                self.focus = Focus::Transcript;
                Effect::None
            }
            KeyCode::Tab => self.cycle(1),
            KeyCode::BackTab => self.cycle(-1),
            KeyCode::Backspace => {
                self.session.draft_mut(self.open).backspace();
                Effect::None
            }
            KeyCode::Delete => {
                self.session.draft_mut(self.open).delete();
                Effect::None
            }
            KeyCode::Right => {
                self.session.draft_mut(self.open).move_by(1);
                Effect::None
            }
            KeyCode::Home => {
                self.session.draft_mut(self.open).cursor = 0;
                Effect::None
            }
            KeyCode::End => {
                let draft = self.session.draft_mut(self.open);
                draft.cursor = draft.text.len();
                Effect::None
            }
            KeyCode::Up => self.composer_vertical(-1),
            KeyCode::Down => self.composer_vertical(1),
            KeyCode::Char(ch) if !ctrl && !ch.is_control() => {
                self.session
                    .draft_mut(self.open)
                    .insert(ch.encode_utf8(&mut [0; 4]));
                Effect::None
            }
            _ => Effect::None,
        }
    }

    fn pane_key(&mut self, key: KeyEvent) -> Effect {
        match key.code {
            KeyCode::Char('q') => Effect::Quit,
            KeyCode::Char('?') => {
                self.help = true;
                Effect::None
            }
            KeyCode::Char('/') => {
                self.focus = Focus::Filter;
                Effect::None
            }
            KeyCode::Char('y') => self.copy_latest(),
            KeyCode::Tab => self.cycle(1),
            KeyCode::BackTab => self.cycle(-1),
            KeyCode::Left | KeyCode::Char('h') => self.horizontal(-1),
            KeyCode::Right | KeyCode::Char('l') => self.horizontal(1),
            KeyCode::Char(' ') => self.toggle(),
            KeyCode::Enter => self.activate(),
            KeyCode::Esc => self.back(),
            KeyCode::Up | KeyCode::Char('k') => self.vertical(-1),
            KeyCode::Down | KeyCode::Char('j') => self.vertical(1),
            KeyCode::Home | KeyCode::Char('g') => self.jump(true),
            KeyCode::End | KeyCode::Char('G') => self.jump(false),
            _ => self.page_key(key, false),
        }
    }

    fn page_key(&mut self, key: KeyEvent, nav: bool) -> Effect {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let up = match key.code {
            KeyCode::PageUp => Some(true),
            KeyCode::PageDown => Some(false),
            KeyCode::Char('u') if ctrl => Some(true),
            KeyCode::Char('d') if ctrl => Some(false),
            _ => None,
        };
        let Some(up) = up else {
            return Effect::None;
        };
        let in_nav = nav || matches!(self.focus, Focus::Nav | Focus::Filter);
        let page = if in_nav {
            self.screen.nav_height
        } else {
            self.screen.transcript_height
        };
        let amount = (page / 2).max(1) as isize;
        let delta = if up { -amount } else { amount };
        if in_nav {
            self.move_nav(delta)
        } else {
            self.scroll_transcript(delta)
        }
    }

    fn cycle(&mut self, delta: isize) -> Effect {
        let order = [Focus::Nav, Focus::Transcript, Focus::Composer];
        let current = match self.focus {
            Focus::Filter => Focus::Nav,
            other => other,
        };
        let index = order.iter().position(|item| *item == current).unwrap_or(0);
        let next = (index as isize + delta).rem_euclid(order.len() as isize) as usize;
        self.focus = order[next];
        Effect::None
    }

    fn horizontal(&mut self, delta: isize) -> Effect {
        if self.focus == Focus::Nav && delta < 0 {
            self.collapse_current();
            return Effect::None;
        }
        if self.focus == Focus::Nav && delta > 0 && self.expand_current() {
            return Effect::None;
        }
        if delta < 0 {
            self.focus = match self.focus {
                Focus::Composer => Focus::Transcript,
                _ => Focus::Nav,
            };
        } else {
            self.focus = match self.focus {
                Focus::Nav => Focus::Transcript,
                _ => Focus::Composer,
            };
        }
        Effect::None
    }

    fn vertical(&mut self, delta: isize) -> Effect {
        if self.focus == Focus::Nav {
            self.move_nav(delta)
        } else {
            self.scroll_transcript(delta)
        }
    }

    fn jump(&mut self, top: bool) -> Effect {
        if self.focus == Focus::Nav {
            self.jump_nav(top)
        } else if top {
            self.follow = false;
            self.scroll = 0;
            self.want_older();
            Effect::None
        } else {
            self.follow = true;
            Effect::None
        }
    }

    fn back(&mut self) -> Effect {
        if !self.filter.is_empty() {
            self.filter.clear();
            self.realign();
            return Effect::None;
        }
        self.focus = match self.focus {
            Focus::Composer => Focus::Transcript,
            Focus::Transcript => Focus::Nav,
            Focus::Nav | Focus::Filter => Focus::Nav,
        };
        Effect::None
    }

    fn activate(&mut self) -> Effect {
        let Some(row) = self.rows().get(self.nav_cursor).cloned() else {
            return Effect::None;
        };
        match row.target {
            Target::Chat(_) => {
                self.focus = Focus::Composer;
                Effect::None
            }
            Target::Dms | Target::Servers | Target::Server(_) => self.toggle(),
        }
    }

    fn toggle(&mut self) -> Effect {
        let Some(row) = self.rows().get(self.nav_cursor).cloned() else {
            return Effect::None;
        };
        match row.target {
            Target::Dms => self.session.dms_open = !self.session.dms_open,
            Target::Servers => self.session.servers_open = !self.session.servers_open,
            Target::Server(id) => {
                if let Some(server) = self.session.server_mut(id) {
                    server.open = !server.open;
                }
            }
            Target::Chat(_) => return Effect::None,
        }
        self.realign();
        Effect::None
    }

    fn collapse_current(&mut self) {
        let Some(row) = self.rows().get(self.nav_cursor).cloned() else {
            return;
        };
        match row.target {
            Target::Dms => self.session.dms_open = false,
            Target::Servers => self.session.servers_open = false,
            Target::Server(id) => {
                if let Some(server) = self.session.server_mut(id) {
                    server.open = false;
                }
            }
            Target::Chat(id) => {
                self.focus_parent(id);
                return;
            }
        }
        self.realign();
    }

    fn expand_current(&mut self) -> bool {
        let Some(row) = self.rows().get(self.nav_cursor).cloned() else {
            return false;
        };
        let changed = match row.target {
            Target::Dms => {
                let was = self.session.dms_open;
                self.session.dms_open = true;
                !was
            }
            Target::Servers => {
                let was = self.session.servers_open;
                self.session.servers_open = true;
                !was
            }
            Target::Server(id) => {
                if let Some(server) = self.session.server_mut(id) {
                    let was = server.open;
                    server.open = true;
                    !was
                } else {
                    false
                }
            }
            Target::Chat(_) => false,
        };
        if changed {
            self.realign();
        }
        changed
    }

    fn focus_parent(&mut self, id: ConvId) {
        if self.session.dms.contains(&id) {
            if let Some(index) = self.rows().iter().position(|row| row.target == Target::Dms) {
                self.nav_cursor = index;
                self.reveal_nav();
            }
            return;
        }
        if let Some(server) = self
            .session
            .servers
            .iter()
            .find(|server| server.channels.contains(&id))
        {
            let target = Target::Server(server.id);
            if let Some(index) = self.rows().iter().position(|row| row.target == target) {
                self.nav_cursor = index;
                self.reveal_nav();
            }
        }
    }

    fn move_nav(&mut self, delta: isize) -> Effect {
        let len = self.rows().len();
        if len == 0 {
            return Effect::None;
        }
        let next = (self.nav_cursor as isize + delta).clamp(0, len as isize - 1) as usize;
        self.nav_cursor = next;
        self.reveal_nav();
        self.open_current();
        Effect::None
    }

    fn jump_nav(&mut self, top: bool) -> Effect {
        let len = self.rows().len();
        if len == 0 {
            return Effect::None;
        }
        self.nav_cursor = if top { 0 } else { len - 1 };
        self.reveal_nav();
        self.open_current();
        Effect::None
    }

    fn open_current(&mut self) {
        let Some(id) = self
            .rows()
            .get(self.nav_cursor)
            .and_then(model::NavRow::chat)
        else {
            return;
        };
        if self.open != id {
            self.open = id;
            self.follow = true;
            self.scroll = 0;
            self.want_history(id);
        }
        self.session.mark_read(id);
    }

    fn reveal_nav(&mut self) {
        let height = self.screen.nav_height;
        if height == 0 {
            return;
        }
        if self.nav_cursor < self.nav_offset {
            self.nav_offset = self.nav_cursor;
        } else if self.nav_cursor >= self.nav_offset + height {
            self.nav_offset = self.nav_cursor + 1 - height;
        }
    }

    fn realign(&mut self) {
        let rows = self.rows();
        if rows.is_empty() {
            self.nav_cursor = 0;
            return;
        }
        if self.nav_cursor >= rows.len() {
            self.nav_cursor = rows.len() - 1;
        }
        self.reveal_nav();
    }

    fn scroll_transcript(&mut self, delta: isize) -> Effect {
        let height = self.screen.transcript_height;
        let total = self.screen.transcript_lines;
        let max = total.saturating_sub(height);
        let current = model::line_window(total, height, self.scroll, self.follow);
        let next = (current as isize + delta).clamp(0, max as isize) as usize;
        self.scroll = next;
        self.follow = next >= max;
        if self.scroll == 0 && !self.follow {
            self.want_older();
        }
        Effect::None
    }

    fn composer_vertical(&mut self, delta: isize) -> Effect {
        let width = self.screen.composer_width.max(1);
        let draft = self.session.draft(self.open);
        let wrap = wrap_draft(&draft.text, draft.cursor, width);
        let target = wrap.cursor_line as isize + delta;
        if target < 0 || target as usize >= wrap.lines.len() {
            return Effect::None;
        }
        let cursor = draft_byte(&draft.text, width, target as usize, wrap.cursor_col);
        self.session.draft_mut(self.open).cursor = cursor;
        Effect::None
    }

    fn send(&mut self) -> Effect {
        let id = self.open;
        if !self.session.visible(id) {
            return Effect::None;
        }
        let body = self.session.draft(id).text.trim().to_string();
        if body.is_empty() {
            return Effect::None;
        }
        if !self.live {
            let time = chrono::Local::now().format("%H:%M").to_string();
            if self.session.send(id, &time) {
                self.follow = true;
            }
            return Effect::None;
        }
        if matches!(self.link, Link::SignedOut) {
            self.toast("Run dt login first", true);
            return Effect::None;
        }
        self.session.draft_mut(id).clear();
        let nonce = discord::nonce();
        let time = chrono::Local::now().format("%H:%M").to_string();
        self.session.messages.entry(id).or_default().push(Message {
            id: 0,
            nonce: Some(nonce.clone()),
            author: self.account.clone(),
            mine: true,
            body: body.clone(),
            time,
            pending: true,
        });
        self.follow = true;
        self.outbox.push(Cmd::Send {
            channel: id.0,
            content: body,
            nonce,
        });
        Effect::None
    }

    fn want_history(&mut self, id: ConvId) {
        if !self.live
            || !self.session.visible(id)
            || self.loaded.contains(&id)
            || self.loading.contains(&id)
        {
            return;
        }
        self.loading.insert(id);
        self.outbox.push(Cmd::History {
            channel: id.0,
            before: None,
        });
    }

    fn want_older(&mut self) {
        let id = self.open;
        if !self.live || !self.session.visible(id) || self.loading.contains(&id) {
            return;
        }
        if !self.loaded.contains(&id) {
            self.want_history(id);
            return;
        }
        if self.exhausted.contains(&id) {
            return;
        }
        let Some(before) = self
            .session
            .messages(id)
            .iter()
            .find_map(|message| (message.id != 0).then_some(message.id))
        else {
            self.exhausted.insert(id);
            return;
        };
        self.loading.insert(id);
        self.outbox.push(Cmd::History {
            channel: id.0,
            before: Some(before),
        });
    }

    fn cursor_target(&self) -> Option<Target> {
        self.rows().get(self.nav_cursor).map(|row| row.target)
    }

    fn after_structure(&mut self, previous: Option<Target>) {
        if !self.session.visible(self.open) {
            self.open = self
                .rows()
                .iter()
                .find_map(model::NavRow::chat)
                .unwrap_or(ConvId(0));
            self.follow = true;
            self.scroll = 0;
            if self.session.visible(self.open) {
                self.session.mark_read(self.open);
                self.want_history(self.open);
            }
            self.point_at_open();
            return;
        }
        if let Some(target) = previous
            && let Some(index) = self.rows().iter().position(|row| row.target == target)
        {
            self.nav_cursor = index;
            self.reveal_nav();
            return;
        }
        self.point_at_open();
    }

    fn point_at_open(&mut self) {
        if let Some(index) = self
            .rows()
            .iter()
            .position(|row| row.chat() == Some(self.open))
        {
            self.nav_cursor = index;
            self.reveal_nav();
        }
    }

    fn install_dms(&mut self, convos: Vec<Convo>) {
        let mut ids = Vec::with_capacity(convos.len());
        for convo in convos {
            ids.push(convo.id);
            self.upsert_convo(convo);
        }
        self.session.dms = ids;
    }

    fn install_guild(&mut self, id: u64, name: String, channels: Vec<Convo>) {
        let open = self
            .session
            .servers
            .iter()
            .find(|server| server.id == id)
            .is_none_or(|server| server.open);
        let mut ids = Vec::with_capacity(channels.len());
        for convo in channels {
            ids.push(convo.id);
            self.upsert_convo(convo);
        }
        if let Some(server) = self.session.server_mut(id) {
            server.name = name;
            server.open = open;
            server.channels = ids;
        } else {
            self.session.servers.push(model::Server {
                id,
                name,
                open,
                channels: ids,
            });
        }
        self.session.servers.sort_by(|left, right| {
            left.name
                .to_lowercase()
                .cmp(&right.name.to_lowercase())
                .then(left.id.cmp(&right.id))
        });
    }

    fn upsert_convo(&mut self, mut convo: Convo) {
        if let Some(existing) = self.session.convos.get(&convo.id) {
            convo.unread = existing.unread;
        }
        self.session.convos.insert(convo.id, convo);
    }

    fn install_channel(&mut self, id: u64, guild: Option<u64>, convo: Option<Convo>) {
        let conv = ConvId(id);
        let Some(convo) = convo else {
            self.session.dms.retain(|item| *item != conv);
            if let Some(guild) = guild
                && let Some(server) = self.session.server_mut(guild)
            {
                server.channels.retain(|item| *item != conv);
            }
            return;
        };
        self.upsert_convo(convo);
        if guild.is_none()
            || self
                .session
                .convos
                .get(&conv)
                .is_some_and(|item| item.kind == Kind::Dm)
        {
            if !self.session.dms.contains(&conv) {
                self.session.dms.insert(0, conv);
            }
            return;
        }
        let Some(guild) = guild else {
            return;
        };
        let Some(server) = self.session.server_mut(guild) else {
            return;
        };
        if !server.channels.contains(&conv) {
            server.channels.push(conv);
        }
    }

    fn install_created(&mut self, channel: u64, message: Message, dm: bool) {
        let id = ConvId(channel);
        if self.session.convo(id).is_none() {
            if !dm {
                return;
            }
            let previous = self.cursor_target();
            self.session.convos.insert(
                id,
                Convo {
                    id,
                    title: "Direct message".into(),
                    kind: Kind::Dm,
                    unread: 0,
                },
            );
            self.session.dms.insert(0, id);
            self.after_structure(previous);
        }
        let same_id = message.id != 0
            && self
                .session
                .messages(id)
                .iter()
                .any(|item| item.id == message.id);
        let same_nonce = message.nonce.as_ref().is_some_and(|nonce| {
            self.session
                .messages(id)
                .iter()
                .any(|item| item.nonce.as_ref() == Some(nonce))
        });
        let duplicate = same_id || same_nonce;
        let mine = message.mine;
        self.session.upsert_message(id, message);
        if self.open == id {
            self.session.mark_read(id);
        } else if !duplicate
            && !mine
            && let Some(convo) = self.session.convos.get_mut(&id)
        {
            convo.unread = convo.unread.saturating_add(1);
        }
    }

    fn install_history(&mut self, channel: u64, older: bool, messages: Vec<Message>, more: bool) {
        let id = ConvId(channel);
        self.loading.remove(&id);
        if messages.is_empty() && older {
            self.loaded.insert(id);
            self.exhausted.insert(id);
            return;
        }
        let width = self.screen.transcript_width.max(1);
        let old_lines = self.session.transcript(id, width).len();
        let existing = self.session.messages(id).to_vec();
        let merged = model::merge_history(&existing, messages, older);
        self.session.messages.insert(id, merged);
        self.loaded.insert(id);
        if !more {
            self.exhausted.insert(id);
        }
        if older && self.open == id && !self.follow {
            let new_lines = self.session.transcript(id, width).len();
            self.scroll = self
                .scroll
                .saturating_add(new_lines.saturating_sub(old_lines));
        }
    }

    fn copy_latest(&mut self) -> Effect {
        let Some(message) = self.session.messages(self.open).last() else {
            self.toast("Nothing to copy", true);
            return Effect::None;
        };
        Effect::Copy {
            text: message.body.clone(),
            toast: "Copied message".to_string(),
        }
    }

    fn wheel(&mut self, column: u16, row: u16, delta: isize) -> Effect {
        if self
            .screen
            .nav_area
            .is_some_and(|area| screen::contains(area, column, row))
        {
            return self.move_nav(delta);
        }
        if self
            .screen
            .transcript
            .is_some_and(|area| screen::contains(area, column, row))
        {
            self.focus = Focus::Transcript;
            return self.scroll_transcript(delta);
        }
        Effect::None
    }

    fn click(&mut self, column: u16, row: u16) -> Effect {
        if self
            .screen
            .filter
            .is_some_and(|area| screen::contains(area, column, row))
        {
            self.focus = Focus::Filter;
            return Effect::None;
        }
        if let Some(index) = screen::hit(&self.screen.nav_hits, column, row) {
            self.focus = Focus::Nav;
            self.nav_cursor = index;
            self.reveal_nav();
            self.open_current();
            return Effect::None;
        }
        if self
            .screen
            .composer
            .is_some_and(|area| screen::contains(area, column, row))
        {
            self.place_composer_cursor(column, row);
            return Effect::None;
        }
        if self
            .screen
            .composer_box
            .is_some_and(|area| screen::contains(area, column, row))
        {
            self.focus = Focus::Composer;
            return Effect::None;
        }
        if self
            .screen
            .transcript
            .is_some_and(|area| screen::contains(area, column, row))
        {
            self.focus = Focus::Transcript;
        }
        Effect::None
    }

    fn place_composer_cursor(&mut self, column: u16, row: u16) {
        self.focus = Focus::Composer;
        let Some(area) = self.screen.composer else {
            return;
        };
        let width = self.screen.composer_width.max(1);
        let line = self.screen.composer_first + row.saturating_sub(area.y) as usize;
        let col = column.saturating_sub(area.x) as usize;
        let text = self.session.draft(self.open).text;
        self.session.draft_mut(self.open).cursor = draft_byte(&text, width, line, col);
    }

    pub fn draft(&self) -> Draft {
        self.session.draft(self.open)
    }

    pub fn status_count(&self) -> String {
        if let Link::Failed(message) = &self.link {
            return message.clone();
        }
        if matches!(self.link, Link::Connecting) {
            return "connecting".into();
        }
        if matches!(self.link, Link::Reconnecting) {
            return "reconnecting".into();
        }
        if matches!(self.link, Link::SignedOut) {
            return "dt login".into();
        }
        if !self.filter.trim().is_empty() {
            let shown = self.rows().len();
            let total = self.session.rows("").len();
            return format!("{shown} of {total}");
        }
        let n = self.session.messages(self.open).len();
        let word = if n == 1 { "message" } else { "messages" };
        format!("{n} {word}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    fn key(ch: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(ch), KeyModifiers::empty())
    }

    fn code(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    #[test]
    fn moving_onto_a_chat_opens_it() {
        let mut app = App::sample();
        assert_eq!(app.open, ConvId(1));
        app.on_key(key('j'));
        assert_eq!(app.open, ConvId(2));
        assert_eq!(app.session.convo(ConvId(2)).unwrap().unread, 0);
    }

    #[test]
    fn space_expands_a_collapsed_server() {
        let mut app = App::sample();
        for _ in 0..8 {
            app.on_key(key('j'));
        }
        assert_eq!(app.rows()[app.nav_cursor].label, "lantern");
        assert!(!app.rows().iter().any(|row| row.label == "specs"));
        app.on_key(code(KeyCode::Char(' ')));
        assert!(app.rows().iter().any(|row| row.label == "specs"));
    }

    #[test]
    fn composer_send_keeps_other_drafts() {
        let mut app = App::sample();
        app.session.draft_mut(ConvId(2)).insert("later");
        app.focus = Focus::Composer;
        app.on_key(key('h'));
        app.on_key(key('i'));
        app.on_key(code(KeyCode::Enter));
        let last = app.session.messages(ConvId(1)).last().unwrap();
        assert_eq!(last.body, "hi");
        assert!(app.session.draft(ConvId(1)).text.is_empty());
        assert_eq!(app.session.draft(ConvId(2)).text, "later");
        assert_eq!(app.focus, Focus::Composer);
    }

    #[test]
    fn esc_and_left_leave_the_composer() {
        let mut app = App::sample();
        app.focus = Focus::Composer;
        app.on_key(code(KeyCode::Left));
        assert_eq!(app.focus, Focus::Transcript);
        app.focus = Focus::Composer;
        app.session.draft_mut(app.open).insert("hi");
        app.on_key(code(KeyCode::Esc));
        assert_eq!(app.focus, Focus::Transcript);
        assert_eq!(app.session.draft(app.open).text, "hi");
    }

    #[test]
    fn q_types_in_the_composer() {
        let mut app = App::sample();
        app.focus = Focus::Composer;
        let effect = app.on_key(key('q'));
        assert!(matches!(effect, Effect::None));
        assert_eq!(app.session.draft(app.open).text, "q");
    }

    #[test]
    fn live_send_restores_the_draft_when_discord_rejects_it() {
        let mut app = App::sample();
        app.live = true;
        app.link = Link::Ready;
        app.account = "bot".into();
        app.focus = Focus::Composer;
        app.on_key(key('h'));
        app.on_key(key('i'));
        app.on_key(code(KeyCode::Enter));
        let last = app.session.messages(ConvId(1)).last().unwrap();
        assert_eq!(last.body, "hi");
        assert!(last.pending);
        let nonce = last.nonce.clone().unwrap();
        assert!(matches!(
            app.outbox.last(),
            Some(Cmd::Send { channel: 1, .. })
        ));
        app.on_event(Ev::SendFailed {
            channel: 1,
            nonce,
            error: "nope".into(),
        });
        assert!(
            app.session
                .messages(ConvId(1))
                .iter()
                .all(|message| message.body != "hi")
        );
        assert_eq!(app.session.draft(ConvId(1)).text, "hi");
    }

    #[test]
    fn a_guild_selects_its_first_channel_and_asks_for_history() {
        let mut app = App::connecting();
        app.on_event(Ev::Ready { name: "dt".into() });
        app.on_event(Ev::Guild(discord::GuildSnap {
            id: 9,
            name: "forge".into(),
            channels: vec![model::Convo {
                id: ConvId(4),
                title: "general".into(),
                kind: model::Kind::Channel,
                unread: 0,
            }],
        }));
        assert_eq!(app.account, "dt");
        assert_eq!(app.open, ConvId(4));
        assert!(matches!(
            app.outbox.last(),
            Some(Cmd::History {
                channel: 4,
                before: None
            })
        ));
        assert!(app.rows().iter().any(|row| row.label == "general"));
    }

    #[test]
    fn filter_narrows_the_list() {
        let mut app = App::sample();
        app.on_key(key('/'));
        app.on_key(key('p'));
        app.on_key(key('r'));
        assert!(app.rows().iter().any(|row| row.label == "Priya"));
        assert!(!app.rows().iter().any(|row| row.label == "forge"));
        app.on_key(code(KeyCode::Esc));
        assert!(app.filter.is_empty());
        assert!(app.rows().iter().any(|row| row.label == "forge"));
    }
}
