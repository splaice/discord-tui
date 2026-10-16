use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use anyhow::Context;

use crate::discord;

pub fn load_token() -> Option<String> {
    if let Ok(raw) = std::env::var("DISCORD_TOKEN") {
        let token = normalize(&raw);
        if !token.is_empty() {
            return Some(token);
        }
    }
    read_token()
}

pub fn login() -> anyhow::Result<()> {
    println!("Paste an account token. README 'Sign in' shows how to copy it from your Discord client.");
    let raw = rpassword::prompt_password("Token: ")?;
    let token = normalize(&raw);
    if !acceptable(&token) {
        anyhow::bail!("That token is empty or has unexpected characters.");
    }
    let identity = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("Could not start a runtime")?
        .block_on(discord::check_token(&token))
        .map_err(anyhow::Error::msg)?;
    save_token(&token)?;
    println!("Signed in as {}.", identity.name);
    println!("Warning: Discord's terms forbid third-party account clients; the account can be suspended.");
    Ok(())
}

pub fn logout() -> anyhow::Result<()> {
    let Some(path) = token_path() else {
        anyhow::bail!("Could not find a home directory");
    };
    match fs::remove_file(&path) {
        Ok(()) => println!("Signed out."),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => println!("No saved token."),
        Err(err) => return Err(err).context(format!("Could not remove {}", path.display())),
    }
    if std::env::var_os("DISCORD_TOKEN").is_some() {
        println!("DISCORD_TOKEN is still set in the environment.");
    }
    Ok(())
}

fn acceptable(token: &str) -> bool {
    !token.is_empty() && token.chars().all(|ch| ch.is_ascii() && !ch.is_whitespace())
}

pub fn normalize(raw: &str) -> String {
    let trimmed = raw.trim();
    trimmed
        .strip_prefix("Bot ")
        .unwrap_or(trimmed)
        .trim()
        .to_string()
}

fn config_dir() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        return Some(PathBuf::from(xdg).join("discord-tui"));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config").join("discord-tui"))
}

fn token_path() -> Option<PathBuf> {
    Some(config_dir()?.join("token"))
}

fn read_token() -> Option<String> {
    let raw = fs::read_to_string(token_path()?).ok()?;
    let token = normalize(&raw);
    acceptable(&token).then_some(token)
}

fn save_token(token: &str) -> anyhow::Result<()> {
    let dir = config_dir().context("Could not find a home directory")?;
    fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    }
    let path = dir.join("token");
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&path)
        .with_context(|| format!("Could not write {}", path.display()))?;
    writeln!(file, "{token}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_a_pasted_prefix() {
        assert_eq!(normalize("  Bot abc.def  "), "abc.def");
        assert_eq!(normalize("abc.def"), "abc.def");
    }
}
