mod app;
mod auth;
mod clipboard;
mod discord;
mod model;
mod screen;
mod text;
mod theme;
mod ui;

use std::io::{self, IsTerminal, Stdout, Write};
use std::sync::mpsc::Receiver;

use tokio::sync::mpsc::UnboundedSender;

use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    KeyEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::app::{App, Effect};
use crate::clipboard::CopyOutcome;
use crate::discord::{Cmd, Ev};

struct Worker {
    cmds: UnboundedSender<Cmd>,
    events: Receiver<Ev>,
}

type Term = Terminal<CrosstermBackend<Stdout>>;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("-h") | Some("--help") | Some("help") => {
            print_help();
            Ok(())
        }
        Some("-V") | Some("--version") => {
            println!("dt {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("login") => {
            if args.next().is_some() {
                eprintln!("dt login does not take arguments");
                std::process::exit(2);
            }
            auth::login()
        }
        Some("logout") => {
            if args.next().is_some() {
                eprintln!("dt logout does not take arguments");
                std::process::exit(2);
            }
            auth::logout()
        }
        Some(other) => {
            eprintln!("unknown command: {other}");
            print_help();
            std::process::exit(2);
        }
        None => {
            if !io::stdout().is_terminal() {
                eprintln!("dt needs an interactive terminal");
                std::process::exit(1);
            }
            enable_raw_mode()?;
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                restore_terminal();
                previous(info);
            }));
            let _guard = Restore;
            execute!(
                io::stdout(),
                EnterAlternateScreen,
                EnableMouseCapture,
                EnableBracketedPaste,
                Hide
            )?;
            let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
            let result = run(&mut terminal);
            drop(terminal);
            result
        }
    }
}

fn print_help() {
    println!(
        "\
dt — a terminal client for Discord chats

Usage:
  dt               Open the client
  dt login         Save an account token
  dt logout        Forget the saved token
  dt help

dt signs in with your Discord account token. It lists your servers
and direct messages, the same ones the Discord apps show.

  dt login         paste an account token (README shows how to copy it)
  tab              chats, messages, composer
  space            fold a section or a server
  enter            open a chat, or send
  ?                keys
  q                quit"
    );
}

fn run(terminal: &mut Term) -> anyhow::Result<()> {
    let worker = auth::load_token().map(|token| {
        let (cmds, events) = discord::spawn(token);
        Worker { cmds, events }
    });
    let mut app = if worker.is_some() {
        App::connecting()
    } else {
        App::signed_out()
    };
    loop {
        if let Some(worker) = &worker {
            while let Ok(event) = worker.events.try_recv() {
                app.on_event(event);
            }
        }
        let mut drawn = screen::Screen::default();
        terminal.draw(|frame| {
            drawn = ui::draw(frame, &app);
        })?;
        app.adopt(drawn);
        flush(&worker, &mut app);

        if crossterm::event::poll(app.poll_timeout())? {
            let effect = match crossterm::event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Release => Effect::None,
                Event::Key(key) => app.on_key(key),
                Event::Mouse(mouse) => app.on_mouse(mouse),
                Event::Paste(text) => app.on_paste(text),
                Event::Resize(_, _) | Event::FocusGained | Event::FocusLost => Effect::None,
            };
            if apply(&mut app, effect) {
                break;
            }
        }
        flush(&worker, &mut app);
        app.tick();
    }
    Ok(())
}

fn flush(worker: &Option<Worker>, app: &mut App) {
    let Some(worker) = worker else {
        app.drain_cmds();
        return;
    };
    for cmd in app.drain_cmds() {
        if worker.cmds.send(cmd).is_err() {
            app.toast("Not connected", true);
            break;
        }
    }
}

fn apply(app: &mut App, effect: Effect) -> bool {
    match effect {
        Effect::None => false,
        Effect::Quit => true,
        Effect::Copy { text, toast } => {
            match clipboard::copy(&text) {
                Ok(CopyOutcome::Native) => app.toast(toast, false),
                Ok(CopyOutcome::BackupOnly { backup }) => app.toast(
                    format!(
                        "Clipboard unavailable. Saved {}",
                        clipboard::display_path(&backup)
                    ),
                    true,
                ),
                Err(err) => app.toast(err, true),
            }
            false
        }
    }
}

struct Restore;

impl Drop for Restore {
    fn drop(&mut self) {
        restore_terminal();
    }
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let mut out = io::stdout();
    let _ = execute!(
        out,
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen,
        Show
    );
    let _ = out.flush();
}
