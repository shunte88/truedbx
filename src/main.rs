/*
 *  main.rs
 *
 *  truedbx - simple dropbox sync service
 *      (c) 2025-26 Stuart Hunter
 *
 *
 *      This program is free software: you can redistribute it and/or modify
 *      it under the terms of the GNU General Public License as published by
 *      the Free Software Foundation, either version 3 of the License, or
 *      (at your option) any later version.
 *
 *      This program is distributed in the hope that it will be useful,
 *      but WITHOUT ANY WARRANTY; without even the implied warranty of
 *      MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 *      GNU General Public License for more details.
 *
 *      See <http://www.gnu.org/licenses/> to get a copy of the GNU General
 *      Public License.
 *
 */
use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod dropbox;
mod error;
mod files;
mod retry;
mod upload_manager;

use error::{TrueDbxError, Result};
use files::scanner::FileScanner;

#[derive(Parser, Debug)]
#[command(name = "truedbx", about = "Upload a file or folder to Dropbox")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Local file or folder to upload
    #[arg(short, long, env = "TDBX_SOURCE")]
    source: Option<PathBuf>,

    /// Dropbox destination folder (missing subfolders are created)
    #[arg(short, long, env = "TDBX_TARGET")]
    target: Option<String>,

    /// Number of concurrent uploads
    #[arg(short, long, env = "TDBX_PARALLEL", default_value_t = 5,
          value_parser = clap::value_parser!(u16).range(1..=100))]
    parallel: u16,

    /// Enable verbose logging
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// One-time: get a refresh token and write it as TDBX_TOKN into an env file (never printed)
    Auth {
        #[arg(long, default_value = ".env")]
        env_file: PathBuf,
    },
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("✗ {}", e);
        std::process::exit(1);
    }
}

/// Credentials come only from the environment, never the command line
fn env(name: &str) -> Result<String> {
    // Tolerate CRLF .env files and quoted values pasted into NAS env UIs
    std::env::var(name)
        .ok()
        .map(|v| v.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| TrueDbxError::Config(format!("{} env var is required", name)))
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    let app_key = env("TDBX_AKEY")?;
    let app_secret = env("TDBX_ASEC")?;

    if let Some(Commands::Auth { env_file }) = &cli.command {
        return auth(&app_key, &app_secret, env_file).await;
    }

    let (Some(source), Some(target)) = (cli.source, cli.target) else {
        return Err(TrueDbxError::Config("--source and --target are required".to_string()));
    };
    let refresh_token = env("TDBX_TOKN")?;
    if !target.starts_with('/') {
        return Err(TrueDbxError::InvalidPath("Target must start with '/'".to_string()));
    }

    // Surface the real reason (not found vs permission denied) instead of a bare "not found"
    std::fs::metadata(&source).map_err(|e| {
        TrueDbxError::Config(format!("Cannot access source {}: {}", source.display(), e))
    })?;

    let scanner = FileScanner::new(source.clone(), target);
    let files = if source.is_dir() {
        scanner.scan_directory(&source)?
    } else {
        scanner.scan_file(&source)?
    };

    // SDK refreshes the short-lived access token itself
    let auth = dropbox_sdk::oauth2::Authorization::from_client_secret_refresh_token(
        app_key,
        app_secret,
        refresh_token,
    );
    let client = dropbox::client::DropboxClient::from_authorization(auth);
    client.validate_token().await?;

    println!("Uploading {} files (parallelism: {})...", files.len(), cli.parallel);
    let manager = upload_manager::UploadManager::new(client, cli.parallel as usize, cli.verbose);
    let stats = manager.upload_files(files).await?;

    println!("\n✓ Uploaded {} files", stats.succeeded);
    if stats.has_failures() {
        return Err(TrueDbxError::Config(format!("{} files failed to upload", stats.failed)));
    }
    Ok(())
}

/// Interactive auth-code flow; writes the refresh token to env_file, never to the terminal
async fn auth(app_key: &str, app_secret: &str, env_file: &std::path::Path) -> Result<()> {
    println!(
        "Visit, click Allow, then paste the code:\n\n  https://www.dropbox.com/oauth2/authorize?client_id={}&response_type=code&token_access_type=offline\n",
        app_key
    );
    let mut code = String::new();
    std::io::stdin().read_line(&mut code)?;
    let code = code.trim();
    if code.is_empty() {
        return Err(TrueDbxError::Config("No code entered (run from an interactive terminal)".to_string()));
    }

    let response: serde_json::Value = reqwest::Client::new()
        .post("https://api.dropboxapi.com/oauth2/token")
        .form(&[
            ("code", code),
            ("grant_type", "authorization_code"),
            ("client_id", app_key),
            ("client_secret", app_secret),
        ])
        .send()
        .await?
        .json()
        .await?;

    // Only echo error fields so no token can leak into the terminal/logs
    let token = response["refresh_token"].as_str().ok_or_else(|| {
        TrueDbxError::Authentication(format!(
            "{} {}",
            response["error"].as_str().unwrap_or("no refresh token returned"),
            response["error_description"].as_str().unwrap_or("")
        ))
    })?;

    write_env_var(env_file, "TDBX_TOKN", token)?;
    println!("✓ TDBX_TOKN written to {}", env_file.display());
    Ok(())
}

/// Set KEY=value in an env file (replacing any existing KEY line), mode 0600
fn write_env_var(path: &std::path::Path, key: &str, value: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let prefix = format!("{}=", key);
    let mut out: String = existing
        .lines()
        .filter(|l| !l.starts_with(&prefix))
        .map(|l| format!("{}\n", l))
        .collect();
    out.push_str(&format!("{}{}\n", prefix, value));

    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(out.as_bytes())?;
    // mode() only applies on create; tighten an existing file too
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_env_var_replaces_existing_key() {
        let path = std::env::temp_dir().join(format!("truedbx-test-{}.env", std::process::id()));
        std::fs::write(&path, "A=1\nTDBX_TOKN=old\nB=2\n").unwrap();
        write_env_var(&path, "TDBX_TOKN", "new").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "A=1\nB=2\nTDBX_TOKN=new\n");
        std::fs::remove_file(&path).unwrap();
    }
}
