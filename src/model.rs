use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use crate::text::wrap_text;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ConvId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Dm,
    Channel,
}

#[derive(Clone, Debug)]
pub struct Convo {
    pub id: ConvId,
    pub title: String,
    pub kind: Kind,
    pub unread: usize,
}

#[derive(Clone, Debug)]
pub struct Server {
    pub id: u64,
    pub name: String,
    pub open: bool,
    pub channels: Vec<ConvId>,
}

#[derive(Clone, Debug)]
pub struct Message {
    pub id: u64,
    pub nonce: Option<String>,
    pub author: String,
    pub mine: bool,
    pub body: String,
    pub time: String,
    pub pending: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Draft {
    pub text: String,
    pub cursor: usize,
}

impl Draft {
    pub fn insert(&mut self, extra: &str) {
        let extra: String = extra
            .chars()
            .filter(|ch| *ch == '\n' || !ch.is_control())
            .take(2000usize.saturating_sub(self.text.chars().count()))
            .collect();
        if extra.is_empty() {
            return;
        }
        self.text.insert_str(self.cursor, &extra);
        self.cursor += extra.len();
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prev = self.text[..self.cursor]
            .chars()
            .next_back()
            .map(|ch| ch.len_utf8())
            .unwrap_or(0);
        self.cursor -= prev;
        self.text.remove(self.cursor);
    }

    pub fn delete(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        self.text.remove(self.cursor);
    }

    pub fn move_by(&mut self, delta: isize) {
        if delta < 0 {
            for _ in 0..(-delta) {
                if self.cursor == 0 {
                    break;
                }
                let prev = self.text[..self.cursor]
                    .chars()
                    .next_back()
                    .map(|ch| ch.len_utf8())
                    .unwrap_or(0);
                self.cursor -= prev;
            }
        } else {
            for _ in 0..delta {
                if self.cursor >= self.text.len() {
                    break;
                }
                let next = self.text[self.cursor..]
                    .chars()
                    .next()
                    .map(|ch| ch.len_utf8())
                    .unwrap_or(0);
                self.cursor += next;
            }
        }
    }

    pub fn kill_word(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let head: String = self.text[..self.cursor].chars().collect();
        let trimmed = head.trim_end();
        let cut = trimmed
            .char_indices()
            .rev()
            .find(|(_, ch)| ch.is_whitespace())
            .map(|(index, ch)| index + ch.len_utf8())
            .unwrap_or(0);
        self.text.replace_range(cut..self.cursor, "");
        self.cursor = cut;
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Dms,
    Servers,
    Server(u64),
    Chat(ConvId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Marker {
    None,
    Open,
    Closed,
    Channel,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavRow {
    pub target: Target,
    pub depth: u8,
    pub marker: Marker,
    pub label: String,
    pub unread: usize,
}

impl NavRow {
    pub fn chat(&self) -> Option<ConvId> {
        match self.target {
            Target::Chat(id) => Some(id),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Meta { mine: bool },
    Body,
    Blank,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TLine {
    pub kind: LineKind,
    pub text: String,
    pub time: String,
}

pub struct Session {
    pub dms_open: bool,
    pub servers_open: bool,
    pub dms: Vec<ConvId>,
    pub servers: Vec<Server>,
    pub convos: HashMap<ConvId, Convo>,
    pub messages: HashMap<ConvId, Vec<Message>>,
    pub drafts: HashMap<ConvId, Draft>,
}

impl Session {
    pub fn convo(&self, id: ConvId) -> Option<&Convo> {
        self.convos.get(&id)
    }

    pub fn messages(&self, id: ConvId) -> &[Message] {
        self.messages.get(&id).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn draft(&self, id: ConvId) -> Draft {
        self.drafts.get(&id).cloned().unwrap_or_default()
    }

    pub fn draft_mut(&mut self, id: ConvId) -> &mut Draft {
        self.drafts.entry(id).or_default()
    }

    pub fn mark_read(&mut self, id: ConvId) {
        if let Some(convo) = self.convos.get_mut(&id) {
            convo.unread = 0;
        }
    }

    pub fn visible(&self, id: ConvId) -> bool {
        self.dms.contains(&id)
            || self
                .servers
                .iter()
                .any(|server| server.channels.contains(&id))
    }

    pub fn server_mut(&mut self, id: u64) -> Option<&mut Server> {
        self.servers.iter_mut().find(|server| server.id == id)
    }

    pub fn restore_draft(&mut self, id: ConvId, text: &str) {
        let draft = self.draft_mut(id);
        if draft.text.is_empty() {
            draft.insert(text);
        }
    }

    pub fn take_pending(&mut self, id: ConvId, nonce: &str) -> Option<Message> {
        let list = self.messages.get_mut(&id)?;
        let index = list
            .iter()
            .position(|message| message.nonce.as_deref() == Some(nonce))?;
        Some(list.remove(index))
    }

    pub fn upsert_message(&mut self, id: ConvId, message: Message) {
        let list = self.messages.entry(id).or_default();
        if let Some(nonce) = message.nonce.clone()
            && let Some(slot) = list
                .iter_mut()
                .find(|item| item.nonce.as_deref() == Some(nonce.as_str()))
        {
            *slot = message;
            return;
        }
        if message.id != 0
            && let Some(slot) = list.iter_mut().find(|item| item.id == message.id)
        {
            *slot = message;
            return;
        }
        list.push(message);
    }

    pub fn send(&mut self, id: ConvId, time: &str) -> bool {
        let Some(draft) = self.drafts.get_mut(&id) else {
            return false;
        };
        let body = draft.text.trim();
        if body.is_empty() {
            return false;
        }
        let body = body.to_string();
        draft.clear();
        self.messages.entry(id).or_default().push(Message {
            id: 0,
            nonce: None,
            author: "you".to_string(),
            mine: true,
            body,
            time: time.to_string(),
            pending: false,
        });
        true
    }

    pub fn chat_count(&self) -> usize {
        self.dms.len()
            + self
                .servers
                .iter()
                .map(|server| server.channels.len())
                .sum::<usize>()
    }

    pub fn rows(&self, filter: &str) -> Vec<NavRow> {
        let needle = filter.trim().to_lowercase();
        if needle.is_empty() {
            return self.tree(false);
        }
        self.filtered(&needle)
    }

    fn tree(&self, force_open: bool) -> Vec<NavRow> {
        let mut rows = Vec::new();
        let dms_open = force_open || self.dms_open;
        rows.push(section(
            Target::Dms,
            "Direct messages",
            dms_open,
            if dms_open {
                0
            } else {
                self.sum_unread(&self.dms)
            },
        ));
        if dms_open {
            for id in &self.dms {
                rows.push(self.chat_row(*id, 1, Marker::None));
            }
        }
        let servers_open = force_open || self.servers_open;
        rows.push(section(
            Target::Servers,
            "Servers",
            servers_open,
            if servers_open {
                0
            } else {
                self.servers
                    .iter()
                    .map(|server| self.sum_unread(&server.channels))
                    .sum()
            },
        ));
        if servers_open {
            for server in &self.servers {
                let open = force_open || server.open;
                rows.push(NavRow {
                    target: Target::Server(server.id),
                    depth: 1,
                    marker: if open { Marker::Open } else { Marker::Closed },
                    label: server.name.clone(),
                    unread: if open {
                        0
                    } else {
                        self.sum_unread(&server.channels)
                    },
                });
                if open {
                    for id in &server.channels {
                        rows.push(self.chat_row(*id, 2, Marker::Channel));
                    }
                }
            }
        }
        rows
    }

    fn filtered(&self, needle: &str) -> Vec<NavRow> {
        let mut rows = Vec::new();
        let dms: Vec<ConvId> = self
            .dms
            .iter()
            .copied()
            .filter(|id| self.matches(*id, needle))
            .collect();
        if !dms.is_empty() {
            rows.push(section(Target::Dms, "Direct messages", true, 0));
            for id in dms {
                rows.push(self.chat_row(id, 1, Marker::None));
            }
        }
        let mut servers = Vec::new();
        for server in &self.servers {
            let server_hit = server.name.to_lowercase().contains(needle);
            let channels: Vec<ConvId> = server
                .channels
                .iter()
                .copied()
                .filter(|id| server_hit || self.matches(*id, needle))
                .collect();
            if !channels.is_empty() {
                servers.push((server.id, server.name.clone(), channels));
            }
        }
        if !servers.is_empty() {
            rows.push(section(Target::Servers, "Servers", true, 0));
            for (id, name, channels) in servers {
                rows.push(NavRow {
                    target: Target::Server(id),
                    depth: 1,
                    marker: Marker::Open,
                    label: name,
                    unread: 0,
                });
                for id in channels {
                    rows.push(self.chat_row(id, 2, Marker::Channel));
                }
            }
        }
        rows
    }

    fn matches(&self, id: ConvId, needle: &str) -> bool {
        self.convos
            .get(&id)
            .is_some_and(|convo| convo.title.to_lowercase().contains(needle))
    }

    fn sum_unread(&self, ids: &[ConvId]) -> usize {
        ids.iter()
            .filter_map(|id| self.convos.get(id).map(|convo| convo.unread))
            .sum()
    }

    fn chat_row(&self, id: ConvId, depth: u8, marker: Marker) -> NavRow {
        let convo = &self.convos[&id];
        NavRow {
            target: Target::Chat(id),
            depth,
            marker,
            label: convo.title.clone(),
            unread: convo.unread,
        }
    }

    pub fn transcript(&self, id: ConvId, width: usize) -> Vec<TLine> {
        let mut lines = Vec::new();
        let messages = self.messages(id);
        for (index, message) in messages.iter().enumerate() {
            lines.push(TLine {
                kind: LineKind::Meta { mine: message.mine },
                text: message.author.clone(),
                time: message.time.clone(),
            });
            let body_width = width.max(1);
            for part in wrap_text(&message.body, body_width) {
                lines.push(TLine {
                    kind: LineKind::Body,
                    text: part,
                    time: String::new(),
                });
            }
            if index + 1 != messages.len() {
                lines.push(TLine {
                    kind: LineKind::Blank,
                    text: String::new(),
                    time: String::new(),
                });
            }
        }
        lines
    }
}

pub fn empty() -> Session {
    Session {
        dms_open: true,
        servers_open: true,
        dms: Vec::new(),
        servers: Vec::new(),
        convos: HashMap::new(),
        messages: HashMap::new(),
        drafts: HashMap::new(),
    }
}

/// Fold a page of history into the messages already on screen.
///
/// `older` prepends a page fetched with `before`. The other path is the first
/// page: keep optimistic sends and any live messages the gateway already delivered.
pub fn merge_history(
    existing: &[Message],
    mut incoming: Vec<Message>,
    older: bool,
) -> Vec<Message> {
    if older {
        let have: HashSet<u64> = existing
            .iter()
            .map(|message| message.id)
            .filter(|id| *id != 0)
            .collect();
        incoming.retain(|message| message.id == 0 || !have.contains(&message.id));
        incoming.extend(existing.iter().cloned());
        return incoming;
    }
    let have: HashSet<u64> = incoming
        .iter()
        .map(|message| message.id)
        .filter(|id| *id != 0)
        .collect();
    let nonces: HashSet<&str> = incoming
        .iter()
        .filter_map(|message| message.nonce.as_deref())
        .collect();
    let mut extra = Vec::new();
    for message in existing {
        if message.pending {
            if message
                .nonce
                .as_deref()
                .is_some_and(|nonce| nonces.contains(nonce))
            {
                continue;
            }
            extra.push(message.clone());
        } else if message.id != 0 && !have.contains(&message.id) {
            extra.push(message.clone());
        }
    }
    incoming.extend(extra);
    incoming.sort_by(|left, right| match (left.pending, right.pending) {
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        _ => left.id.cmp(&right.id),
    });
    incoming
}

fn section(target: Target, label: &str, open: bool, unread: usize) -> NavRow {
    NavRow {
        target,
        depth: 0,
        marker: if open { Marker::Open } else { Marker::Closed },
        label: label.to_string(),
        unread,
    }
}

pub fn line_window(total: usize, height: usize, scroll: usize, follow: bool) -> usize {
    let max = total.saturating_sub(height);
    if follow { max } else { scroll.min(max) }
}

#[cfg(test)]
pub fn sample() -> Session {
    let mara = ConvId(1);
    let jonah = ConvId(2);
    let priya = ConvId(3);
    let general = ConvId(4);
    let help = ConvId(5);
    let releases = ConvId(6);
    let lantern_general = ConvId(7);
    let specs = ConvId(8);

    let mut convos = HashMap::new();
    let mut messages = HashMap::new();
    let add = |convos: &mut HashMap<ConvId, Convo>,
               id: ConvId,
               title: &str,
               kind: Kind,
               unread: usize| {
        convos.insert(
            id,
            Convo {
                id,
                title: title.to_string(),
                kind,
                unread,
            },
        );
    };
    add(&mut convos, mara, "Mara", Kind::Dm, 0);
    add(&mut convos, jonah, "Jonah", Kind::Dm, 2);
    add(&mut convos, priya, "Priya", Kind::Dm, 0);
    add(&mut convos, general, "general", Kind::Channel, 0);
    add(&mut convos, help, "help", Kind::Channel, 1);
    add(&mut convos, releases, "releases", Kind::Channel, 0);
    add(&mut convos, lantern_general, "general", Kind::Channel, 0);
    add(&mut convos, specs, "specs", Kind::Channel, 4);

    messages.insert(
        mara,
        vec![
            message("Mara", false, "09:14", "The header spacing on the list still feels tight."),
            message("you", true, "09:16", "I'll match the inbox padding and send a shot."),
            message(
                "Mara",
                false,
                "09:18",
                "The accent on the focused row is the part I care about. Everything else can stay quiet.",
            ),
        ],
    );
    messages.insert(
        jonah,
        vec![
            message(
                "Jonah",
                false,
                "08:02",
                "Are you around for the gateway notes?",
            ),
            message("Jonah", false, "08:11", "No rush. I left them in #specs."),
        ],
    );
    messages.insert(
        priya,
        vec![
            message("Priya", false, "12:40", "Shipping the composer as its own pane."),
            message(
                "you",
                true,
                "12:44",
                "Enter sends. A newline is ctrl-j, so the box can hold a short paragraph without leaving.",
            ),
        ],
    );
    messages.insert(
        general,
        vec![
            message("Lena", false, "11:02", "Deploy is green."),
            message(
                "Sam",
                false,
                "11:05",
                "Watching the reconnect path. If the socket drops, the transcript should stay where you left it.",
            ),
        ],
    );
    messages.insert(
        help,
        vec![message(
            "Nico",
            false,
            "10:22",
            "How do I fold a server so the channel list stays short?",
        )],
    );
    messages.insert(
        releases,
        vec![message(
            "Lena",
            false,
            "16:20",
            "Cut is the text client: servers, channels, and direct messages. Voice can wait.",
        )],
    );
    messages.insert(
        lantern_general,
        vec![message(
            "Ada",
            false,
            "15:01",
            "Lantern is quiet today. Specs are the active room.",
        )],
    );
    messages.insert(
        specs,
        vec![
            message(
                "Jonah",
                false,
                "08:40",
                "Gateway notes: identify, then listen. History loads when a channel is opened, not for every room at startup.",
            ),
            message(
                "Ada",
                false,
                "08:46",
                "And the composer stays on screen while the history scrolls.",
            ),
        ],
    );

    Session {
        dms_open: true,
        servers_open: true,
        dms: vec![mara, jonah, priya],
        servers: vec![
            Server {
                id: 100,
                name: "forge".to_string(),
                open: true,
                channels: vec![general, help, releases],
            },
            Server {
                id: 101,
                name: "lantern".to_string(),
                open: false,
                channels: vec![lantern_general, specs],
            },
        ],
        convos,
        messages,
        drafts: HashMap::new(),
    }
}

#[cfg(test)]
fn message(author: &str, mine: bool, time: &str, body: &str) -> Message {
    Message {
        id: 0,
        nonce: None,
        author: author.to_string(),
        mine,
        body: body.to_string(),
        time: time.to_string(),
        pending: false,
    }
}

pub fn placeholder(convo: &Convo) -> String {
    match convo.kind {
        Kind::Dm => format!("Message {}", convo.title),
        Kind::Channel => format!("Message #{}", convo.title),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_hides_a_collapsed_server() {
        let session = sample();
        let rows = session.rows("");
        assert!(rows.iter().any(|row| row.label == "releases"));
        assert!(!rows.iter().any(|row| row.label == "specs"));
        assert!(
            rows.iter()
                .any(|row| row.label == "lantern" && row.unread == 4)
        );
    }

    #[test]
    fn filter_opens_matching_channels() {
        let session = sample();
        let rows = session.rows("spec");
        assert!(rows.iter().any(|row| row.label == "specs"));
        assert!(!rows.iter().any(|row| row.label == "Mara"));
    }

    #[test]
    fn drafts_stay_with_their_chat() {
        let mut session = sample();
        session.draft_mut(ConvId(1)).insert("hello");
        session.draft_mut(ConvId(2)).insert("other");
        assert_eq!(session.draft(ConvId(1)).text, "hello");
        assert!(session.send(ConvId(1), "18:00"));
        assert!(session.draft(ConvId(1)).text.is_empty());
        assert_eq!(session.draft(ConvId(2)).text, "other");
        assert_eq!(session.messages(ConvId(1)).last().unwrap().body, "hello");
    }

    #[test]
    fn follow_pins_the_tail() {
        assert_eq!(line_window(20, 5, 0, true), 15);
        assert_eq!(line_window(20, 5, 3, false), 3);
        assert_eq!(line_window(20, 5, 100, false), 15);
    }

    #[test]
    fn server_rows_use_stable_ids() {
        let rows = sample().rows("");
        assert!(
            rows.iter()
                .any(|row| row.label == "forge" && row.target == Target::Server(100))
        );
        assert!(
            rows.iter()
                .any(|row| row.label == "lantern" && row.target == Target::Server(101))
        );
    }

    #[test]
    fn nonce_replaces_the_optimistic_message() {
        let mut session = sample();
        session
            .messages
            .entry(ConvId(1))
            .or_default()
            .push(Message {
                id: 0,
                nonce: Some("n1".into()),
                author: "bot".into(),
                mine: true,
                body: "hi".into(),
                time: "18:00".into(),
                pending: true,
            });
        session.upsert_message(
            ConvId(1),
            Message {
                id: 50,
                nonce: Some("n1".into()),
                author: "bot".into(),
                mine: true,
                body: "hi".into(),
                time: "18:00".into(),
                pending: false,
            },
        );
        let messages = session.messages(ConvId(1));
        assert_eq!(
            messages
                .iter()
                .filter(|message| message.body == "hi")
                .count(),
            1
        );
        assert_eq!(messages.last().unwrap().id, 50);
    }

    #[test]
    fn older_history_is_prepended_without_duplicates() {
        let existing = vec![message_id(10, "new")];
        let incoming = vec![message_id(9, "old"), message_id(10, "new")];
        let merged = merge_history(&existing, incoming, true);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].id, 9);
        assert_eq!(merged[1].id, 10);
    }

    fn message_id(id: u64, body: &str) -> Message {
        let mut message = message("ada", false, "12:00", body);
        message.id = id;
        message
    }
}
