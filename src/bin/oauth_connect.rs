//! `oauth-connect` — small CLI binary that drives a native OAuth PKCE
//! handshake against a real provider and persists the resulting tokens
//! into the same `AuthService` the desktop app uses.
//!
//! Built as a separate `[[bin]]` rather than a CLI subcommand so it
//! does not bloat the core's main `cli.rs` dispatcher; it can be
//! retired once the proper JSON-RPC controller (`openhuman.oauth_*`)
//! lands and the frontend can trigger flows directly.
//!
//! Usage:
//!
//! ```text
//!   OPENHUMAN_GOOGLE_OAUTH_CLIENT_ID=… \
//!   cargo run --bin oauth-connect -- --provider google
//!   #  optional: --workspace <path>  (defaults to $OPENHUMAN_WORKSPACE
//!   #  or ~/.openhuman)
//!   #  optional: --profile <name>    (defaults to "default")
//!   #  optional: --timeout-secs <N>  (defaults to 300)
//! ```
//!
//! The binary prints the consent URL, opens it in the system browser
//! when possible, and blocks waiting for the loopback callback. On
//! success it prints the connected provider + profile id. On failure
//! it prints the typed `OAuthFlowError` verbatim.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use clap::Parser;

use openhuman_core::openhuman::credentials::AuthService;
use openhuman_core::openhuman::oauth::ops::{start_github_flow, start_google_flow};

#[derive(Parser, Debug)]
#[command(
    name = "oauth-connect",
    about = "Run a native OAuth PKCE flow and persist tokens locally"
)]
struct Args {
    /// Provider to connect (`google` or `github`).
    #[arg(long)]
    provider: String,

    /// Override the workspace dir (otherwise `$OPENHUMAN_WORKSPACE`
    /// or `~/.openhuman`).
    #[arg(long)]
    workspace: Option<PathBuf>,

    /// Profile name to store under in the encrypted profile store.
    #[arg(long, default_value = "default")]
    profile: String,

    /// How long to wait for the loopback callback before giving up.
    #[arg(long, default_value_t = 300)]
    timeout_secs: u64,
}

fn workspace_dir(arg: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = arg {
        return Ok(p);
    }
    if let Ok(env) = std::env::var("OPENHUMAN_WORKSPACE") {
        return Ok(PathBuf::from(env));
    }
    let home = dirs::home_dir()
        .ok_or_else(|| anyhow!("cannot resolve home directory; pass --workspace explicitly"))?;
    Ok(home.join(".openhuman"))
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .try_init()
        .ok();

    let args = Args::parse();

    let workspace = workspace_dir(args.workspace)?;
    let state_dir = workspace.join("state");
    std::fs::create_dir_all(&state_dir)
        .with_context(|| format!("creating state dir {}", state_dir.display()))?;
    // Force encrypted-at-rest persistence to match the scope decision
    // in tasks/todo.md (config.secrets.encrypt = true). Local testing
    // can override by editing this binary if needed; production use is
    // always encrypted.
    let service = AuthService::new(&state_dir, true);
    let http = reqwest::Client::new();
    let timeout = Duration::from_secs(args.timeout_secs);

    let flow = match args.provider.as_str() {
        "google" => start_google_flow(http).await.map_err(|e| anyhow!("{e}"))?,
        "github" => start_github_flow(http).await.map_err(|e| anyhow!("{e}"))?,
        other => {
            return Err(anyhow!(
                "unknown provider '{other}' — expected one of: google, github"
            ));
        }
    };

    println!();
    println!("[oauth-connect] Workspace:    {}", workspace.display());
    println!("[oauth-connect] Loopback:     {}", flow.redirect_uri);
    println!("[oauth-connect] Provider:     {}", args.provider);
    println!();
    println!("Open this URL in your browser to authorize:");
    println!();
    println!("  {}", flow.auth_url);
    println!();

    // Best-effort: try to launch the system browser. `webbrowser` is
    // NOT a dep here, so we fall back to printing the URL only —
    // copy/paste works fine for the validation use case.
    if let Err(e) = open_in_browser(&flow.auth_url) {
        log::debug!("[oauth-connect] could not open browser ({e}); awaiting manual paste");
    }

    println!(
        "Waiting for callback ({} seconds; Ctrl-C to abort)…",
        args.timeout_secs
    );

    let completion = flow
        .complete(&service, &args.profile, timeout)
        .await
        .map_err(|e| anyhow!("{e}"))?;

    println!();
    println!("✓ Connected.");
    println!("  provider     = {}", completion.provider);
    println!("  profile.id   = {}", completion.profile.id);
    println!("  profile_name = {}", completion.profile.profile_name);
    if let Some(ts) = &completion.profile.token_set {
        if let Some(exp) = &ts.expires_at {
            println!("  expires_at   = {exp}");
        }
        if let Some(scope) = &ts.scope {
            println!("  scope        = {scope}");
        }
    }
    println!();
    println!(
        "Tokens stored under: {}/{{auth profile store}}",
        state_dir.display()
    );

    Ok(())
}

/// Best-effort open of `url` in the system browser. Falls back to a
/// platform-specific shell call. Returns an error if no opener
/// command is available so the caller can surface a clean log line
/// instead of treating the failure as fatal.
fn open_in_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    let prog = "open";
    #[cfg(target_os = "linux")]
    let prog = "xdg-open";
    #[cfg(target_os = "windows")]
    let prog = "explorer";
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let prog = {
        return Err(anyhow!("no known browser-opener for this platform"));
    };
    std::process::Command::new(prog)
        .arg(url)
        .spawn()
        .map_err(|e| anyhow!("spawning {prog} to open browser: {e}"))?;
    Ok(())
}
