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

    #[arg(long, env = "TDBX_APP_KEY", hide_env_values = true)]
    app_key: String,

    #[arg(long, env = "TDBX_APP_SECRET", hide_env_values = true)]
    app_secret: String,

    #[arg(long, env = "TDBX_REFRESH_TOKEN", hide_env_values = true)]
    refresh_token: Option<String>,

    /// Enable verbose logging
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// One-time: exchange an authorization code for a refresh token to put in .env
    Auth,
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("✗ {}", e);
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();

    if let Some(Commands::Auth) = cli.command {
        return auth(&cli.app_key, &cli.app_secret).await;
    }

    let (Some(source), Some(target), Some(refresh_token)) = (cli.source, cli.target, cli.refresh_token) else {
        return Err(TrueDbxError::Config(
            "--source, --target and TDBX_REFRESH_TOKEN are required".to_string(),
        ));
    };
    if !target.starts_with('/') {
        return Err(TrueDbxError::InvalidPath("Target must start with '/'".to_string()));
    }

    let scanner = FileScanner::new(source.clone(), target);
    let files = if source.is_dir() {
        scanner.scan_directory(&source)?
    } else {
        scanner.scan_file(&source)?
    };

    // SDK refreshes the short-lived access token itself
    let auth = dropbox_sdk::oauth2::Authorization::from_client_secret_refresh_token(
        cli.app_key,
        cli.app_secret,
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

/// Interactive auth-code flow; prints the refresh token rather than storing it
async fn auth(app_key: &str, app_secret: &str) -> Result<()> {
    println!(
        "Visit, click Allow, then paste the code:\n\n  https://www.dropbox.com/oauth2/authorize?client_id={}&response_type=code&token_access_type=offline\n",
        app_key
    );
    let mut code = String::new();
    std::io::stdin().read_line(&mut code)?;

    let response: serde_json::Value = reqwest::Client::new()
        .post("https://api.dropboxapi.com/oauth2/token")
        .form(&[
            ("code", code.trim()),
            ("grant_type", "authorization_code"),
            ("client_id", app_key),
            ("client_secret", app_secret),
        ])
        .send()
        .await?
        .json()
        .await?;

    let token = response["refresh_token"]
        .as_str()
        .ok_or_else(|| TrueDbxError::Authentication(format!("No refresh token: {}", response)))?;
    println!("\nAdd to .env:\n\nTDBX_REFRESH_TOKEN={}", token);
    Ok(())
}
