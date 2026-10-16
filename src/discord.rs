//! Discord client for a user account.
//!
//! The token is the user's own account token, copied from a Discord client.
//! It authenticates REST and gateway calls without the `Bot ` prefix, and it
//! carries no intents: user sessions always receive message content.
//!
//! Note: Discord's terms forbid using an account with third-party clients;
//! the account is at risk of suspension.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async_with_config};

use crate::model::{ConvId, Convo, Kind, Message};

const API: &str = "https://discord.com/api/v10";
const PAGE: usize = 50;

static NONCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub struct GuildSnap {
    pub id: u64,
    pub name: String,
    pub channels: Vec<Convo>,
}

pub enum Cmd {
    Send {
        channel: u64,
        content: String,
        nonce: String,
    },
    History {
        channel: u64,
        before: Option<u64>,
    },
}

#[derive(Debug)]
pub enum Ev {
    Connecting,
    Reconnecting,
    Ready {
        name: String,
    },
    Failed(String),
    Dms(Vec<Convo>),
    Guild(GuildSnap),
    GuildName {
        id: u64,
        name: String,
    },
    GuildRemoved(u64),
    Channel {
        id: u64,
        guild: Option<u64>,
        convo: Option<Convo>,
    },
    History {
        channel: u64,
        older: bool,
        messages: Vec<Message>,
        more: bool,
    },
    HistoryFailed {
        channel: u64,
        error: String,
    },
    Created {
        channel: u64,
        message: Message,
        dm: bool,
    },
    Edited {
        channel: u64,
        message: u64,
        body: String,
    },
    Deleted {
        channel: u64,
        message: u64,
    },
    SendFailed {
        channel: u64,
        nonce: String,
        error: String,
    },
    Notice(String),
}

#[derive(Debug)]
pub struct Identity {
    pub name: String,
}

pub fn spawn(token: String) -> (UnboundedSender<Cmd>, std::sync::mpsc::Receiver<Ev>) {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let (ev_tx, ev_rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("discord".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(err) => {
                    let _ = ev_tx.send(Ev::Failed(err.to_string()));
                    return;
                }
            };
            runtime.block_on(serve(token, cmd_rx, ev_tx));
        })
        .expect("discord thread");
    (cmd_tx, ev_rx)
}

pub fn nonce() -> String {
    let tick = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0);
    let mixed = tick
        ^ NONCE
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15);
    format!("{mixed:x}")
}

pub async fn check_token(token: &str) -> Result<Identity, String> {
    let client = http()?;
    let user = api_retry(&client, token, reqwest::Method::GET, "/users/@me", None)
        .await
        .map_err(|err| err.message())?;
    Ok(Identity {
        name: display_name(&user),
    })
}

struct ResumeState {
    session_id: String,
    origin: String,
    seq: u64,
}

struct Live {
    resume: Option<ResumeState>,
    account: String,
    self_id: u64,
}

enum Step {
    Stop,
    Reconnect {
        keep_resume: bool,
        established: bool,
        error: Option<String>,
    },
}

enum Action {
    Continue,
    Heartbeat,
    Reconnect {
        keep_resume: bool,
        error: Option<String>,
    },
    Stop,
}

#[derive(Debug)]
enum ApiError {
    Retry(f64),
    Fatal(String),
    Soft(String),
}

impl ApiError {
    fn message(self) -> String {
        match self {
            ApiError::Retry(_) => "Discord rate limit".into(),
            ApiError::Fatal(message) | ApiError::Soft(message) => message,
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Retry(_) => write!(formatter, "Discord rate limit"),
            ApiError::Fatal(message) | ApiError::Soft(message) => write!(formatter, "{message}"),
        }
    }
}

async fn serve(token: String, mut cmds: UnboundedReceiver<Cmd>, ev: Sender<Ev>) {
    let client = match http() {
        Ok(client) => client,
        Err(err) => {
            let _ = ev.send(Ev::Failed(err));
            return;
        }
    };
    let mut live = Live {
        resume: None,
        account: String::new(),
        self_id: 0,
    };
    let mut fails = 0u32;
    loop {
        let reconnecting = live.resume.is_some();
        let _ = ev.send(if reconnecting {
            Ev::Reconnecting
        } else {
            Ev::Connecting
        });
        let origin = if let Some(state) = &live.resume {
            state.origin.clone()
        } else {
            match gateway_origin(&client, &token).await {
                Ok(origin) => origin,
                Err(ApiError::Fatal(message)) => {
                    let _ = ev.send(Ev::Failed(message));
                    return;
                }
                Err(err) => {
                    fails = fails.saturating_add(1);
                    note_failure(&ev, fails, &err.to_string());
                    tokio::time::sleep(backoff(fails)).await;
                    continue;
                }
            }
        };
        match connect_session(
            &client,
            &token,
            &gateway_ws(&origin),
            &mut live,
            &mut cmds,
            &ev,
        )
        .await
        {
            Step::Stop => return,
            Step::Reconnect {
                keep_resume,
                established,
                error,
            } => {
                if !keep_resume {
                    live.resume = None;
                }
                if established {
                    fails = 0;
                }
                fails = fails.saturating_add(1);
                if let Some(error) = error {
                    note_failure(&ev, fails, &error);
                } else if fails >= 3 {
                    note_failure(&ev, fails, "Reconnecting");
                }
                tokio::time::sleep(backoff(fails)).await;
            }
        }
    }
}

fn note_failure(ev: &Sender<Ev>, fails: u32, message: &str) {
    if fails >= 2 {
        let _ = ev.send(Ev::Failed(message.to_string()));
    }
}

fn backoff(fails: u32) -> Duration {
    let shift = fails.saturating_sub(1).min(4);
    Duration::from_secs((1u64 << shift).min(15))
}

async fn connect_session(
    client: &reqwest::Client,
    token: &str,
    url: &str,
    live: &mut Live,
    cmds: &mut UnboundedReceiver<Cmd>,
    ev: &Sender<Ev>,
) -> Step {
    let config = WebSocketConfig::default()
        .max_message_size(Some(64 * 1024 * 1024))
        .max_frame_size(Some(64 * 1024 * 1024));
    let connected = tokio::time::timeout(
        Duration::from_secs(20),
        connect_async_with_config(url, Some(config), true),
    )
    .await;
    let ws = match connected {
        Ok(Ok((ws, _))) => ws,
        Ok(Err(err)) => {
            return Step::Reconnect {
                keep_resume: true,
                established: false,
                error: Some(redact(&err.to_string(), token)),
            };
        }
        Err(_) => {
            return Step::Reconnect {
                keep_resume: true,
                established: false,
                error: Some("Gateway connection timed out".into()),
            };
        }
    };
    let (mut write, mut read) = ws.split();
    let hello = match next_text(&mut read).await {
        Ok(text) => text,
        Err(err) => {
            return Step::Reconnect {
                keep_resume: true,
                established: false,
                error: Some(redact(&err, token)),
            };
        }
    };
    let Some(interval) = heartbeat_interval(&hello) else {
        return Step::Reconnect {
            keep_resume: false,
            established: false,
            error: Some("Gateway hello was missing a heartbeat".into()),
        };
    };
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel();
    let hello_payload = match &live.resume {
        Some(state) => resume_payload(token, &state.session_id, state.seq),
        None => identify_payload(token),
    };
    let _ = out_tx.send(hello_payload);
    let mut seq = live.resume.as_ref().map(|state| state.seq);
    let mut next_heartbeat = Instant::now() + interval / 2;
    let mut waiting_ack = false;
    let mut established = false;
    loop {
        let idle = next_heartbeat.saturating_duration_since(Instant::now());
        tokio::select! {
            _ = tokio::time::sleep(idle) => {
                if waiting_ack {
                    return Step::Reconnect {
                        keep_resume: true,
                        established,
                        error: Some("Heartbeat timed out".into()),
                    };
                }
                next_heartbeat = Instant::now() + interval;
                let payload = heartbeat_payload(seq);
                if write.send(WsMessage::Text(payload.into())).await.is_err() {
                    return Step::Reconnect {
                        keep_resume: true,
                        established,
                        error: Some("Gateway write failed".into()),
                    };
                }
                waiting_ack = true;
            }
            incoming = read.next() => {
                let Some(incoming) = incoming else {
                    return Step::Reconnect { keep_resume: true, established, error: Some("Gateway closed".into()) };
                };
                let message = match incoming {
                    Ok(message) => message,
                    Err(err) => {
                        return Step::Reconnect {
                            keep_resume: true,
                            established,
                            error: Some(redact(&err.to_string(), token)),
                        };
                    }
                };
                match on_socket(message, token, live, ev, &mut seq, &mut waiting_ack, &mut established) {
                    Action::Continue => {}
                    Action::Heartbeat => {
                        let _ = out_tx.send(heartbeat_payload(seq));
                    }
                    Action::Stop => return Step::Stop,
                    Action::Reconnect { keep_resume, error } => {
                        return Step::Reconnect { keep_resume, established, error };
                    }
                }
            }
            command = cmds.recv() => {
                let Some(command) = command else {
                    return Step::Stop;
                };
                spawn_cmd(client.clone(), token.to_string(), ev.clone(), live.self_id, command);
            }
            outgoing = out_rx.recv() => {
                let Some(text) = outgoing else {
                    return Step::Stop;
                };
                if write.send(WsMessage::Text(text.into())).await.is_err() {
                    return Step::Reconnect { keep_resume: true, established, error: Some("Gateway write failed".into()) };
                }
            }
        }
    }
}

fn on_socket(
    message: WsMessage,
    token: &str,
    live: &mut Live,
    ev: &Sender<Ev>,
    seq: &mut Option<u64>,
    waiting_ack: &mut bool,
    established: &mut bool,
) -> Action {
    match message {
        WsMessage::Text(text) => on_text(&text, token, live, ev, seq, waiting_ack, established),
        WsMessage::Close(frame) => {
            let code = frame
                .as_ref()
                .map(|frame| u16::from(frame.code))
                .unwrap_or(1000);
            if let Some(message) = fatal_close(code) {
                let _ = ev.send(Ev::Failed(message.into()));
                return Action::Stop;
            }
            Action::Reconnect {
                keep_resume: resume_after_close(code),
                error: Some(format!("Gateway closed ({code})")),
            }
        }
        WsMessage::Ping(_) | WsMessage::Pong(_) | WsMessage::Binary(_) | WsMessage::Frame(_) => {
            Action::Continue
        }
    }
}

fn on_text(
    text: &str,
    token: &str,
    live: &mut Live,
    ev: &Sender<Ev>,
    seq: &mut Option<u64>,
    waiting_ack: &mut bool,
    established: &mut bool,
) -> Action {
    let payload: Value = match serde_json::from_str(text) {
        Ok(payload) => payload,
        Err(_) => return Action::Continue,
    };
    let op = payload.get("op").and_then(|op| op.as_u64()).unwrap_or(99);
    match op {
        11 => *waiting_ack = false,
        7 => {
            return Action::Reconnect {
                keep_resume: true,
                error: None,
            };
        }
        9 => {
            let keep_resume = payload
                .get("d")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            if !keep_resume {
                live.resume = None;
            }
            return Action::Reconnect {
                keep_resume,
                error: Some("Session ended".into()),
            };
        }
        1 => return Action::Heartbeat,
        0 => {
            if let Some(next) = payload.get("s").and_then(|value| value.as_u64()) {
                *seq = Some(next);
                if let Some(state) = live.resume.as_mut() {
                    state.seq = next;
                }
            }
            dispatch(&payload, token, live, ev, established);
        }
        _ => {}
    }
    Action::Continue
}

fn dispatch(
    payload: &Value,
    token: &str,
    live: &mut Live,
    ev: &Sender<Ev>,
    established: &mut bool,
) {
    let kind = payload
        .get("t")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let data = payload.get("d").cloned().unwrap_or(Value::Null);
    match kind {
        "READY" => {
            *established = true;
            live.self_id = field_snowflake(data.get("user").unwrap_or(&Value::Null), "id");
            live.account = data
                .get("user")
                .map(display_name)
                .unwrap_or_else(|| "bot".into());
            let session_id = data
                .get("session_id")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            let origin = data
                .get("resume_gateway_url")
                .and_then(|value| value.as_str())
                .unwrap_or("wss://gateway.discord.gg")
                .to_string();
            let seq = payload
                .get("s")
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            if !session_id.is_empty() {
                live.resume = Some(ResumeState {
                    session_id,
                    origin,
                    seq,
                });
            }
            let _ = ev.send(Ev::Ready {
                name: live.account.clone(),
            });
            let client = http().ok();
            if let Some(client) = client {
                let token = token.to_string();
                tokio::spawn({
                    let client = client.clone();
                    let token = token.clone();
                    let ev = ev.clone();
                    async move {
                        load_dms(&client, &token, &ev).await;
                    }
                });
                tokio::spawn({
                    let ev = ev.clone();
                    async move {
                        load_guilds(&client, &token, &ev).await;
                    }
                });
            }
        }
        "RESUMED" => {
            *established = true;
            if !live.account.is_empty() {
                let _ = ev.send(Ev::Ready {
                    name: live.account.clone(),
                });
            }
        }
        "GUILD_CREATE" => {
            if let Some(guild) = map_guild(&data) {
                // User-account gateway payloads omit the channel list; the
                // REST load on READY supplies it. A create without channels
                // only refreshes the name, so it cannot wipe loaded channels.
                let has_channels = data
                    .get("channels")
                    .and_then(|item| item.as_array())
                    .map(|items| !items.is_empty())
                    .unwrap_or(false);
                if has_channels {
                    let _ = ev.send(Ev::Guild(guild));
                } else if !guild.name.is_empty() {
                    let _ = ev.send(Ev::GuildName {
                        id: guild.id,
                        name: guild.name,
                    });
                }
            }
        }
        "GUILD_UPDATE" => {
            let id = field_snowflake(&data, "id");
            let name = data
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            if id != 0 && !name.is_empty() {
                let _ = ev.send(Ev::GuildName {
                    id,
                    name: name.to_string(),
                });
            }
        }
        "GUILD_DELETE" => {
            if data
                .get("unavailable")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
            {
                return;
            }
            let id = field_snowflake(&data, "id");
            if id != 0 {
                let _ = ev.send(Ev::GuildRemoved(id));
            }
        }
        "CHANNEL_CREATE" | "CHANNEL_UPDATE" => {
            if let Some(event) = map_channel_event(&data, true) {
                let _ = ev.send(event);
            }
        }
        "CHANNEL_DELETE" => {
            if let Some(event) = map_channel_event(&data, false) {
                let _ = ev.send(event);
            }
        }
        "MESSAGE_CREATE" => {
            if let Some(event) = map_created(&data, live.self_id) {
                let _ = ev.send(event);
            }
        }
        "MESSAGE_UPDATE" => {
            if let Some(event) = map_edited(&data) {
                let _ = ev.send(event);
            }
        }
        "MESSAGE_DELETE" => {
            let channel = field_snowflake(&data, "channel_id");
            let message = field_snowflake(&data, "id");
            if channel != 0 && message != 0 {
                let _ = ev.send(Ev::Deleted { channel, message });
            }
        }
        _ => {}
    }
}

fn spawn_cmd(client: reqwest::Client, token: String, ev: Sender<Ev>, self_id: u64, cmd: Cmd) {
    tokio::spawn(async move {
        match cmd {
            Cmd::Send {
                channel,
                content,
                nonce,
            } => send_one(&client, &token, &ev, channel, content, nonce).await,
            Cmd::History { channel, before } => {
                load_history(&client, &token, &ev, self_id, channel, before).await
            }
        }
    });
}

async fn send_one(
    client: &reqwest::Client,
    token: &str,
    ev: &Sender<Ev>,
    channel: u64,
    content: String,
    nonce: String,
) {
    let body = serde_json::json!({ "content": content, "nonce": nonce });
    let path = format!("/channels/{channel}/messages");
    if let Err(err) = api_retry(client, token, reqwest::Method::POST, &path, Some(&body)).await {
        if matches!(err, ApiError::Fatal(_)) {
            let _ = ev.send(Ev::Failed(err.to_string()));
        }
        let _ = ev.send(Ev::SendFailed {
            channel,
            nonce,
            error: err.message(),
        });
    }
}

async fn load_history(
    client: &reqwest::Client,
    token: &str,
    ev: &Sender<Ev>,
    self_id: u64,
    channel: u64,
    before: Option<u64>,
) {
    let path = match before {
        Some(id) => format!("/channels/{channel}/messages?limit={PAGE}&before={id}"),
        None => format!("/channels/{channel}/messages?limit={PAGE}"),
    };
    match api_retry(client, token, reqwest::Method::GET, &path, None).await {
        Ok(value) => {
            let messages = page_messages(&value, self_id);
            let more = value
                .as_array()
                .map(|items| items.len() == PAGE)
                .unwrap_or(false);
            let _ = ev.send(Ev::History {
                channel,
                older: before.is_some(),
                messages,
                more,
            });
        }
        Err(err) => {
            if matches!(err, ApiError::Fatal(_)) {
                let _ = ev.send(Ev::Failed(err.to_string()));
            }
            let _ = ev.send(Ev::HistoryFailed {
                channel,
                error: err.message(),
            });
        }
    }
}

async fn load_dms(client: &reqwest::Client, token: &str, ev: &Sender<Ev>) {
    match api_retry(
        client,
        token,
        reqwest::Method::GET,
        "/users/@me/channels",
        None,
    )
    .await
    {
        Ok(Value::Array(channels)) => {
            let mut dms: Vec<(u64, Convo)> = channels
                .iter()
                .filter_map(|channel| {
                    let convo = dm_convo(channel)?;
                    Some((field_snowflake(channel, "last_message_id"), convo))
                })
                .collect();
            dms.sort_by_key(|item| std::cmp::Reverse(item.0));
            let _ = ev.send(Ev::Dms(dms.into_iter().map(|(_, convo)| convo).collect()));
        }
        Ok(_) => {}
        Err(ApiError::Fatal(message)) => {
            let _ = ev.send(Ev::Failed(message));
        }
        Err(err) => {
            let _ = ev.send(Ev::Notice(err.message()));
        }
    }
}

/// User accounts receive guild stubs over the gateway without channels, so the
/// list comes from REST: one paginated pass for names, then channels per guild.
async fn load_guilds(client: &reqwest::Client, token: &str, ev: &Sender<Ev>) {
    let mut guilds: Vec<(u64, String)> = Vec::new();
    let mut after: u64 = 0;
    loop {
        let path = if after == 0 {
            "/users/@me/guilds?limit=200".to_string()
        } else {
            format!("/users/@me/guilds?limit=200&after={after}")
        };
        match api_retry(client, token, reqwest::Method::GET, &path, None).await {
            Ok(Value::Array(page)) => {
                for guild in &page {
                    let id = field_snowflake(guild, "id");
                    if id != 0 {
                        let name = guild
                            .get("name")
                            .and_then(|item| item.as_str())
                            .filter(|name| !name.is_empty())
                            .unwrap_or("server")
                            .to_string();
                        guilds.push((id, name));
                    }
                }
                match page.last().map(|guild| field_snowflake(guild, "id")) {
                    Some(id) if id > after && page.len() >= 200 => after = id,
                    _ => break,
                }
            }
            Ok(_) => break,
            Err(ApiError::Fatal(message)) => {
                let _ = ev.send(Ev::Failed(message));
                return;
            }
            Err(err) => {
                let _ = ev.send(Ev::Notice(err.message()));
                return;
            }
        }
    }
    for (id, name) in &guilds {
        let _ = ev.send(Ev::Guild(GuildSnap {
            id: *id,
            name: name.clone(),
            channels: Vec::new(),
        }));
    }
    for (id, name) in guilds {
        let path = format!("/guilds/{id}/channels");
        match api_retry(client, token, reqwest::Method::GET, &path, None).await {
            Ok(value) => {
                let channels = value
                    .as_array()
                    .map(|items| text_channels(items))
                    .unwrap_or_default();
                let _ = ev.send(Ev::Guild(GuildSnap { id, name, channels }));
            }
            Err(err) => {
                let _ = ev.send(Ev::Notice(format!("{name}: {}", err.message())));
            }
        }
    }
}

async fn next_text(
    read: &mut futures_util::stream::SplitStream<
        WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    >,
) -> Result<String, String> {
    let framed = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match read.next().await {
                Some(Ok(WsMessage::Text(text))) => return Ok(text.to_string()),
                Some(Ok(WsMessage::Ping(_) | WsMessage::Pong(_) | WsMessage::Frame(_))) => {}
                Some(Ok(WsMessage::Binary(_))) => {}
                Some(Ok(WsMessage::Close(frame))) => {
                    let code = frame
                        .as_ref()
                        .map(|frame| u16::from(frame.code))
                        .unwrap_or(1000);
                    return Err(fatal_close(code).unwrap_or("Gateway closed").to_string());
                }
                Some(Err(err)) => return Err(err.to_string()),
                None => return Err("Gateway closed".into()),
            }
        }
    })
    .await;
    match framed {
        Ok(result) => result,
        Err(_) => Err("Gateway hello timed out".into()),
    }
}

fn heartbeat_interval(text: &str) -> Option<Duration> {
    let payload: Value = serde_json::from_str(text).ok()?;
    if payload.get("op").and_then(|op| op.as_u64()) != Some(10) {
        return None;
    }
    let millis = payload
        .pointer("/d/heartbeat_interval")
        .and_then(|value| value.as_u64())?;
    Some(Duration::from_millis(millis.max(1_000)))
}

fn identify_payload(token: &str) -> String {
    serde_json::json!({
        "op": 2,
        "d": {
            "token": token,
            "properties": {
                "os": std::env::consts::OS,
                "browser": "dt",
                "device": "dt"
            }
        }
    })
    .to_string()
}

fn resume_payload(token: &str, session_id: &str, seq: u64) -> String {
    serde_json::json!({
        "op": 6,
        "d": { "token": token, "session_id": session_id, "seq": seq }
    })
    .to_string()
}

fn heartbeat_payload(seq: Option<u64>) -> String {
    serde_json::json!({ "op": 1, "d": seq }).to_string()
}

fn http() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(format!("dt/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|err| err.to_string())
}

async fn gateway_origin(client: &reqwest::Client, token: &str) -> Result<String, ApiError> {
    let value = api_retry(client, token, reqwest::Method::GET, "/gateway", None).await?;
    value
        .get("url")
        .and_then(|url| url.as_str())
        .filter(|url| !url.is_empty())
        .map(str::to_string)
        .ok_or_else(|| ApiError::Soft("Gateway response had no url".into()))
}

async fn api_retry(
    client: &reqwest::Client,
    token: &str,
    method: reqwest::Method,
    path: &str,
    body: Option<&Value>,
) -> Result<Value, ApiError> {
    match api(client, token, method.clone(), path, body).await {
        Err(ApiError::Retry(secs)) => {
            tokio::time::sleep(Duration::from_secs_f64(secs.clamp(0.05, 5.0))).await;
            match api(client, token, method, path, body).await {
                Err(ApiError::Retry(_)) => Err(ApiError::Soft("Discord rate limit".into())),
                other => other,
            }
        }
        other => other,
    }
}

async fn api(
    client: &reqwest::Client,
    token: &str,
    method: reqwest::Method,
    path: &str,
    body: Option<&Value>,
) -> Result<Value, ApiError> {
    let url = format!("{API}{path}");
    let mut request = client
        .request(method, &url)
        .header("Authorization", token);
    if let Some(body) = body {
        request = request.json(body);
    }
    let response = match request.send().await {
        Ok(response) => response,
        Err(err) => return Err(ApiError::Soft(redact(&err.to_string(), token))),
    };
    let status = response.status();
    if status.as_u16() == 429 {
        let value = response.json::<Value>().await.unwrap_or(Value::Null);
        let wait = value
            .get("retry_after")
            .and_then(|value| value.as_f64())
            .unwrap_or(1.0);
        return Err(ApiError::Retry(wait));
    }
    let text = response.text().await.unwrap_or_default();
    if status.as_u16() == 401 {
        return Err(ApiError::Fatal(
            "Discord rejected the token. Run dt login.".into(),
        ));
    }
    if !status.is_success() {
        let message = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|value| {
                value
                    .get("message")
                    .and_then(|item| item.as_str())
                    .map(str::to_string)
            })
            .filter(|message| !message.is_empty())
            .unwrap_or_else(|| format!("Discord returned {}", status.as_u16()));
        return Err(ApiError::Soft(redact(&message, token)));
    }
    if text.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&text).map_err(|err| ApiError::Soft(redact(&err.to_string(), token)))
}

fn gateway_ws(origin: &str) -> String {
    let origin = origin.trim_end_matches('/');
    if origin.contains('?') {
        format!("{origin}&v=10&encoding=json")
    } else {
        format!("{origin}/?v=10&encoding=json")
    }
}

fn redact(text: &str, token: &str) -> String {
    if token.is_empty() {
        text.to_string()
    } else {
        text.replace(token, "[token]")
    }
}

fn fatal_close(code: u16) -> Option<&'static str> {
    match code {
        4004 => Some("Discord rejected the token. Run dt login."),
        4010..=4012 => Some("Discord closed the session."),
        _ => None,
    }
}

fn resume_after_close(code: u16) -> bool {
    !matches!(code, 4007 | 4009)
}

fn map_guild(value: &Value) -> Option<GuildSnap> {
    if value
        .get("unavailable")
        .and_then(|item| item.as_bool())
        .unwrap_or(false)
    {
        return None;
    }
    let id = field_snowflake(value, "id");
    if id == 0 {
        return None;
    }
    let name = value
        .get("name")
        .and_then(|item| item.as_str())
        .unwrap_or("server")
        .to_string();
    let channels = value
        .get("channels")
        .and_then(|item| item.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    Some(GuildSnap {
        id,
        name,
        channels: text_channels(channels),
    })
}

fn text_channels(channels: &[Value]) -> Vec<Convo> {
    let categories: HashMap<u64, i64> = channels
        .iter()
        .filter_map(|channel| {
            if channel.get("type").and_then(|item| item.as_u64()) != Some(4) {
                return None;
            }
            let id = field_snowflake(channel, "id");
            (id != 0).then_some((id, position(channel)))
        })
        .collect();
    let mut texts: Vec<&Value> = channels
        .iter()
        .filter(|channel| {
            matches!(
                channel.get("type").and_then(|item| item.as_u64()),
                Some(0 | 5)
            )
        })
        .collect();
    texts.sort_by_key(|channel| {
        let parent = field_snowflake(channel, "parent_id");
        let category = categories.get(&parent).copied().unwrap_or(-1);
        (category, position(channel), field_snowflake(channel, "id"))
    });
    texts
        .iter()
        .filter_map(|channel| guild_convo(channel))
        .collect()
}

fn map_channel_event(value: &Value, keep: bool) -> Option<Ev> {
    let id = field_snowflake(value, "id");
    if id == 0 {
        return None;
    }
    let guild = value.get("guild_id").map(snowflake).filter(|id| *id != 0);
    let kind = value.get("type").and_then(|item| item.as_u64());
    let shown = matches!(kind, Some(0 | 1 | 3 | 5));
    if !keep || !shown {
        return shown.then_some(Ev::Channel {
            id,
            guild,
            convo: None,
        });
    }
    let convo = if matches!(kind, Some(1 | 3)) {
        dm_convo(value)
    } else {
        guild_convo(value)
    };
    Some(Ev::Channel { id, guild, convo })
}

fn guild_convo(value: &Value) -> Option<Convo> {
    let id = field_snowflake(value, "id");
    if id == 0 {
        return None;
    }
    let title = value
        .get("name")
        .and_then(|item| item.as_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("channel");
    Some(Convo {
        id: ConvId(id),
        title: title.to_string(),
        kind: Kind::Channel,
        unread: 0,
    })
}

fn dm_convo(value: &Value) -> Option<Convo> {
    let id = field_snowflake(value, "id");
    if id == 0
        || !matches!(
            value.get("type").and_then(|item| item.as_u64()),
            Some(1 | 3)
        )
    {
        return None;
    }
    Some(Convo {
        id: ConvId(id),
        title: dm_title(value),
        kind: Kind::Dm,
        unread: 0,
    })
}

fn dm_title(value: &Value) -> String {
    if value.get("type").and_then(|item| item.as_u64()) == Some(3)
        && let Some(name) = value.get("name").and_then(|item| item.as_str())
        && !name.is_empty()
    {
        return name.to_string();
    }
    let names: Vec<String> = value
        .get("recipients")
        .and_then(|item| item.as_array())
        .map(|people| {
            people
                .iter()
                .map(display_name)
                .filter(|name| name != "unknown")
                .collect()
        })
        .unwrap_or_default();
    if names.is_empty() {
        value
            .get("name")
            .and_then(|item| item.as_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("Direct message")
            .to_string()
    } else {
        names.join(", ")
    }
}

fn map_created(value: &Value, self_id: u64) -> Option<Ev> {
    let channel = field_snowflake(value, "channel_id");
    let message = map_message(value, self_id)?;
    if channel == 0 {
        return None;
    }
    let dm = value.get("guild_id").is_none();
    Some(Ev::Created {
        channel,
        message,
        dm,
    })
}

fn map_edited(value: &Value) -> Option<Ev> {
    if value.get("content").is_none() && value.get("attachments").is_none() {
        return None;
    }
    let channel = field_snowflake(value, "channel_id");
    let message = field_snowflake(value, "id");
    if channel == 0 || message == 0 {
        return None;
    }
    Some(Ev::Edited {
        channel,
        message,
        body: message_body(value),
    })
}

fn page_messages(value: &Value, self_id: u64) -> Vec<Message> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .rev()
                .filter_map(|item| map_message(item, self_id))
                .collect()
        })
        .unwrap_or_default()
}

fn map_message(value: &Value, self_id: u64) -> Option<Message> {
    let id = field_snowflake(value, "id");
    if id == 0 {
        return None;
    }
    let author = value.get("author").unwrap_or(&Value::Null);
    let author_id = field_snowflake(author, "id");
    Some(Message {
        id,
        nonce: nonce_of(value),
        author: display_name(author),
        mine: self_id != 0 && author_id == self_id,
        body: message_body(value),
        time: format_time(
            value
                .get("timestamp")
                .and_then(|item| item.as_str())
                .unwrap_or(""),
        ),
        pending: false,
    })
}

fn message_body(value: &Value) -> String {
    let content = value
        .get("content")
        .and_then(|item| item.as_str())
        .unwrap_or("")
        .trim();
    if !content.is_empty() {
        return content.to_string();
    }
    let files: Vec<&str> = value
        .get("attachments")
        .and_then(|item| item.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|item| item.get("filename").and_then(|name| name.as_str()))
                .collect()
        })
        .unwrap_or_default();
    if !files.is_empty() {
        return files.join(" ");
    }
    if let Some(title) = value
        .pointer("/embeds/0/title")
        .and_then(|item| item.as_str())
        && !title.is_empty()
    {
        return title.to_string();
    }
    "(no text)".into()
}

fn display_name(user: &Value) -> String {
    let global = user
        .get("global_name")
        .and_then(|item| item.as_str())
        .unwrap_or("");
    if !global.is_empty() {
        return global.to_string();
    }
    user.get("username")
        .and_then(|item| item.as_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("unknown")
        .to_string()
}

fn nonce_of(value: &Value) -> Option<String> {
    match value.get("nonce")? {
        Value::String(nonce) if !nonce.is_empty() => Some(nonce.clone()),
        Value::Number(nonce) => Some(nonce.to_string()),
        _ => None,
    }
}

fn format_time(raw: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

fn field_snowflake(value: &Value, key: &str) -> u64 {
    value.get(key).map(snowflake).unwrap_or(0)
}

fn snowflake(value: &Value) -> u64 {
    match value {
        Value::String(text) => text.parse().unwrap_or(0),
        Value::Number(number) => number.as_u64().unwrap_or(0),
        _ => 0,
    }
}

fn position(value: &Value) -> i64 {
    value
        .get("position")
        .and_then(|item| item.as_i64())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_url_pins_json_v10() {
        assert_eq!(
            gateway_ws("wss://gateway.discord.gg"),
            "wss://gateway.discord.gg/?v=10&encoding=json"
        );
        assert_eq!(
            gateway_ws("wss://gateway.discord.gg/?v=9"),
            "wss://gateway.discord.gg/?v=9&v=10&encoding=json"
        );
    }

    #[test]
    fn token_is_stripped_from_errors() {
        assert_eq!(redact("bad abc.def token", "abc.def"), "bad [token] token");
    }

    #[test]
    fn nonce_fits_discord_limit() {
        let nonce = nonce();
        assert!(nonce.len() <= 25, "{nonce}");
        assert!(!nonce.is_empty());
    }

    #[test]
    fn message_uses_global_name_and_marks_mine() {
        let value = serde_json::json!({
            "id": "10",
            "channel_id": "4",
            "content": "hello",
            "timestamp": "2024-01-01T00:00:00+00:00",
            "nonce": "n1",
            "author": { "id": "7", "username": "botuser", "global_name": "Ada" }
        });
        let message = map_message(&value, 7).unwrap();
        assert_eq!(message.author, "Ada");
        assert!(message.mine);
        assert_eq!(message.nonce.as_deref(), Some("n1"));
        assert_eq!(message.body, "hello");
    }

    #[test]
    fn empty_content_uses_the_filename() {
        let value = serde_json::json!({
            "content": "",
            "attachments": [{ "filename": "shot.png" }]
        });
        assert_eq!(message_body(&value), "shot.png");
    }

    #[test]
    fn history_page_is_oldest_first() {
        let value = serde_json::json!([
            { "id": "3", "content": "new", "timestamp": "2024-01-01T00:00:00Z", "author": { "id": "1", "username": "a" } },
            { "id": "2", "content": "old", "timestamp": "2024-01-01T00:00:00Z", "author": { "id": "1", "username": "a" } }
        ]);
        let messages = page_messages(&value, 9);
        assert_eq!(messages[0].body, "old");
        assert_eq!(messages[1].body, "new");
        assert!(!messages[0].mine);
    }

    #[test]
    fn channels_follow_category_order() {
        let channels = vec![
            serde_json::json!({"id": "1", "type": 4, "name": "later", "position": 2}),
            serde_json::json!({"id": "2", "type": 4, "name": "first", "position": 0}),
            serde_json::json!({"id": "3", "type": 0, "name": "loose", "position": 5}),
            serde_json::json!({"id": "4", "type": 0, "name": "general", "position": 1, "parent_id": "2"}),
            serde_json::json!({"id": "5", "type": 2, "name": "voice", "position": 0}),
            serde_json::json!({"id": "6", "type": 5, "name": "news", "position": 0, "parent_id": "1"}),
        ];
        let names: Vec<_> = text_channels(&channels)
            .into_iter()
            .map(|convo| convo.title)
            .collect();
        assert_eq!(names, vec!["loose", "general", "news"]);
    }

    #[test]
    fn direct_message_prefers_global_name() {
        let value = serde_json::json!({
            "id": "8",
            "type": 1,
            "recipients": [{ "id": "2", "username": "m", "global_name": "Mara" }]
        });
        assert_eq!(dm_convo(&value).unwrap().title, "Mara");
    }

    #[test]
    fn disallowed_intent_does_not_reconnect() {
        assert!(fatal_close(4004).is_some());
        assert!(fatal_close(4013).is_none());
        assert!(fatal_close(4014).is_none());
        assert!(fatal_close(4000).is_none());
        assert!(!resume_after_close(4007));
        assert!(resume_after_close(4000));
    }
}
