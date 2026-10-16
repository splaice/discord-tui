use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

#[derive(Debug)]
pub enum CopyOutcome {
    Native,
    BackupOnly { backup: PathBuf },
}

pub fn copy(text: &str) -> Result<CopyOutcome, String> {
    let backup = write_backup(text);
    if try_native(text) || write_osc52(text) {
        return Ok(CopyOutcome::Native);
    }
    match backup {
        Some(backup) => Ok(CopyOutcome::BackupOnly { backup }),
        None => Err("Clipboard unavailable".to_string()),
    }
}

pub fn display_path(path: &std::path::Path) -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        if let Ok(rest) = path.strip_prefix(&home) {
            return format!("~/{}", rest.display());
        }
    }
    path.display().to_string()
}

fn backup_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join(".discord-tui")
            .join("last-copy.txt"),
    )
}

fn write_backup(text: &str) -> Option<PathBuf> {
    let path = backup_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    std::fs::write(&path, text).ok()?;
    Some(path)
}

fn try_native(text: &str) -> bool {
    let attempts: &[(&str, &[&str])] = &[
        ("pbcopy", &[]),
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--input"]),
    ];
    for (bin, args) in attempts {
        let Ok(mut child) = Command::new(bin)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        let Some(mut stdin) = child.stdin.take() else {
            let _ = child.kill();
            continue;
        };
        if stdin.write_all(text.as_bytes()).is_err() {
            drop(stdin);
            let _ = child.kill();
            continue;
        }
        drop(stdin);
        if wait_child(&mut child, Duration::from_secs(2)) {
            return true;
        }
    }
    false
}

fn wait_child(child: &mut std::process::Child, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if start.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => return false,
        }
    }
}

fn write_osc52(text: &str) -> bool {
    let encoded = STANDARD.encode(text.as_bytes());
    let osc = format!("\u{1b}]52;c;{encoded}\u{7}");
    let seq = if std::env::var_os("TMUX").is_some() {
        format!("\u{1b}Ptmux;\u{1b}{osc}\u{1b}\\")
    } else {
        osc
    };
    let Ok(mut tty) = std::fs::OpenOptions::new().write(true).open("/dev/tty") else {
        return false;
    };
    tty.write_all(seq.as_bytes()).is_ok() && tty.flush().is_ok()
}
