//! Process-wide registry of channel listener task handles.
//!
//! Lets `connect_channel` / `disconnect_channel` RPCs abort and respawn a
//! specific channel's supervised listener without restarting the whole
//! `openhuman-core` process. Initialised once from `start_channels` with
//! the shared message-dispatch sender and supervisor backoff config;
//! subsequent calls go through the singleton.
//!
//! Scope: covers the three channels currently exposed via the
//! UI's connect/disconnect surface (telegram, discord, imessage). Other
//! channel types still require a process restart on config change —
//! their RPCs don't go through this path.
//!
//! What hot-reload does NOT cover (today):
//!
//! * `ChannelRuntimeContext::channels_by_name` is built once at startup
//!   and held by subscribers (proactive, cron, etc.) as an immutable
//!   `Arc<HashMap>`. A rebuilt listener uses the *new* token for
//!   *inbound* polling, but agent-driven *outbound* replies still go
//!   through the old `Arc<dyn Channel>` registered at startup. For the
//!   same-token reconnect case (the user's 409 scenario) this is
//!   irrelevant — both Arcs point at the same bot. Changing the bot
//!   token mid-process leaves outbound sends going to the old bot
//!   until the process restarts. Fixing that requires swapping
//!   `channels_by_name` to an `ArcSwap` or similar; tracked as a
//!   follow-up.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::supervision::spawn_supervised_listener;
use crate::openhuman::channels::discord::DiscordChannel;
use crate::openhuman::channels::imessage::IMessageChannel;
use crate::openhuman::channels::telegram::TelegramChannel;
use crate::openhuman::channels::traits::ChannelMessage;
use crate::openhuman::channels::Channel;
use crate::openhuman::config::Config;

struct RegistryInner {
    handles: Mutex<HashMap<String, JoinHandle<()>>>,
    tx: mpsc::Sender<ChannelMessage>,
    initial_backoff_secs: u64,
    max_backoff_secs: u64,
}

static REGISTRY: OnceLock<RegistryInner> = OnceLock::new();

/// One-time initialization called from `start_channels`. Idempotent —
/// subsequent calls are silently ignored so the supervisor keeps the
/// first sender. Safe to call on every `start_channels` invocation.
pub(crate) fn init(
    tx: mpsc::Sender<ChannelMessage>,
    initial_backoff_secs: u64,
    max_backoff_secs: u64,
) {
    let _ = REGISTRY.set(RegistryInner {
        handles: Mutex::new(HashMap::new()),
        tx,
        initial_backoff_secs,
        max_backoff_secs,
    });
}

/// Register an already-spawned listener handle so it can be aborted
/// later. Called by `start_channels` for each channel it boots.
/// Replacing an existing entry aborts the prior handle.
pub(crate) fn track(channel_id: String, handle: JoinHandle<()>) {
    let Some(r) = REGISTRY.get() else {
        tracing::warn!(
            channel = %channel_id,
            "[listener-registry] track called before init; handle leaked"
        );
        return;
    };
    if let Some(prev) = r.handles.lock().insert(channel_id.clone(), handle) {
        tracing::warn!(
            channel = %channel_id,
            "[listener-registry] replacing existing handle; aborting prior"
        );
        prev.abort();
    }
}

/// Abort the listener for `channel_id` if one is running. Returns true
/// when an active handle was found and aborted. The aborted task
/// terminates at its next .await point — for Telegram's `getUpdates`
/// long-poll that's at most one HTTP-poll cycle (typically <30s, but
/// the channel's 409 handler keeps the poll body short).
pub fn abort(channel_id: &str) -> bool {
    let Some(r) = REGISTRY.get() else {
        return false;
    };
    let Some(handle) = r.handles.lock().remove(channel_id) else {
        return false;
    };
    handle.abort();
    tracing::info!(
        channel = %channel_id,
        "[listener-registry] aborted existing listener"
    );
    true
}

/// Abort any existing listener for `channel_id` and spawn a fresh
/// supervised listener built from the current `config`. Returns true
/// when a new listener was spawned, false when the channel is no
/// longer configured (the entry was removed from
/// `config.channels_config.<channel>`) or the registry is uninitialised.
pub fn rebuild(channel_id: &str, config: &Config) -> bool {
    abort(channel_id);

    let Some(channel) = build_channel_from_config(channel_id, config) else {
        tracing::info!(
            channel = %channel_id,
            "[listener-registry] channel absent from config after disconnect; no new listener spawned"
        );
        return false;
    };

    let Some(r) = REGISTRY.get() else {
        tracing::warn!(
            channel = %channel_id,
            "[listener-registry] registry not initialised; cannot spawn (start_channels must run first)"
        );
        return false;
    };

    let handle = spawn_supervised_listener(
        channel,
        r.tx.clone(),
        r.initial_backoff_secs,
        r.max_backoff_secs,
    );
    r.handles.lock().insert(channel_id.to_string(), handle);
    tracing::info!(
        channel = %channel_id,
        "[listener-registry] spawned fresh listener from current config"
    );
    true
}

/// Build a single channel instance from its slot in
/// `config.channels_config`. Returns `None` when the channel is absent
/// from the config (e.g., just disconnected). Only covers the channel
/// IDs that flow through the UI's connect/disconnect RPC surface —
/// other channels still go through `start_channels`'s big match.
fn build_channel_from_config(channel_id: &str, config: &Config) -> Option<Arc<dyn Channel>> {
    match channel_id {
        "telegram" => config.channels_config.telegram.as_ref().map(|tg| {
            Arc::new(
                TelegramChannel::new(
                    tg.bot_token.clone(),
                    tg.allowed_users.clone(),
                    tg.mention_only,
                )
                .with_streaming(
                    tg.stream_mode,
                    tg.draft_update_interval_ms,
                    tg.silent_streaming,
                ),
            ) as Arc<dyn Channel>
        }),
        "discord" => config.channels_config.discord.as_ref().map(|dc| {
            Arc::new(DiscordChannel::new(
                dc.bot_token.clone(),
                dc.guild_id.clone(),
                dc.channel_id.clone(),
                dc.allowed_users.clone(),
                dc.listen_to_bots,
                dc.mention_only,
            )) as Arc<dyn Channel>
        }),
        "imessage" => config.channels_config.imessage.as_ref().map(|im| {
            Arc::new(IMessageChannel::new(im.allowed_contacts.clone())) as Arc<dyn Channel>
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openhuman::config::{Config, TelegramConfig};

    fn telegram_config(token: &str) -> Config {
        let mut c = Config::default();
        c.channels_config.telegram = Some(TelegramConfig {
            bot_token: token.to_string(),
            allowed_users: Vec::new(),
            stream_mode: crate::openhuman::config::StreamMode::default(),
            draft_update_interval_ms: 1000,
            silent_streaming: true,
            mention_only: false,
        });
        c
    }

    #[test]
    fn abort_returns_false_when_no_handle_tracked() {
        // Fresh test runtime — no listener has ever been tracked for
        // "telegram-test-unused", so abort is a clean no-op.
        assert!(!abort("telegram-test-unused"));
    }

    #[test]
    fn build_channel_from_config_returns_none_for_absent() {
        let cfg = Config::default(); // no channels configured
        assert!(build_channel_from_config("telegram", &cfg).is_none());
        assert!(build_channel_from_config("discord", &cfg).is_none());
        assert!(build_channel_from_config("imessage", &cfg).is_none());
    }

    #[test]
    fn build_channel_from_config_returns_some_for_telegram_when_configured() {
        let cfg = telegram_config("test-token-redacted");
        let ch = build_channel_from_config("telegram", &cfg).expect("must build telegram channel");
        assert_eq!(ch.name(), "telegram");
    }

    #[test]
    fn build_channel_from_config_returns_none_for_unknown_channel() {
        let cfg = telegram_config("test-token-redacted");
        // The registry covers telegram/discord/imessage; other channel
        // ids stay on the start_channels boot path.
        assert!(build_channel_from_config("slack", &cfg).is_none());
        assert!(build_channel_from_config("nonsense", &cfg).is_none());
    }
}
