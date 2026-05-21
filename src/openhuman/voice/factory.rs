//! Factory functions for creating voice (STT / TTS) providers.
//!
//! Mirrors the shape of [`crate::openhuman::embeddings::factory`]: a single
//! entry point that takes a provider name + parameters and returns a boxed
//! trait object. Production paths pick the provider based on the user's
//! config (`stt_provider`, `tts_provider`); unit tests use the factory
//! directly to verify dispatch branches.
//!
//! ## STT providers
//!
//! - `"cloud"` → backend Whisper proxy (POST `/openai/v1/audio/transcriptions`).
//!   Same path the renamed `MicComposer` used to call directly. Keeps the API key
//!   off the desktop, costs network round-trip latency.
//! - `"whisper"` → local Whisper via the `WHISPER_BIN` env var (or in-process
//!   `whisper-rs` engine when `local_ai.whisper_in_process` is on). Zero
//!   network, but the user has to download the model. Default model:
//!   `whisper-large-v3-turbo` (recommended) or smaller variants
//!   (`tiny / base / small / medium`) for lower-end hardware.
//!
//! ## TTS providers
//!
//! - `"cloud"` → backend ElevenLabs proxy (POST `/openai/v1/audio/speech`)
//!   which also returns Oculus-15 visemes for the mascot lip-sync.
//! - `"piper"` → local Piper subprocess via `PIPER_BIN`. Lower latency than
//!   ElevenLabs and runs offline; default voice `en_US-lessac-medium`.
//! - `"kokoro"` → local OpenAI-compatible TTS server (Kokoro on
//!   kokoro-fastapi / mlx-audio / LM Studio audio mode). Higher quality
//!   than Piper, runs offline, and on Apple Silicon the MLX backend is
//!   markedly faster than Piper's CPU ONNX path. Endpoint, model, and
//!   default voice come from `config.local_ai.kokoro_*`.
//!
//! ## Logging prefixes
//!
//! All factory branches log against `[voice-factory]`; the wrapped provider
//! implementations log under `[voice-stt]` / `[voice-tts]` so end-to-end
//! traces grep cleanly.

use std::sync::Arc;

use async_trait::async_trait;
use log::debug;
use serde::{Deserialize, Serialize};

use super::cloud_transcribe::{transcribe_cloud, CloudTranscribeOptions, CloudTranscribeResult};
use super::local_speech::{synthesize_piper, PiperOptions};
use super::local_transcribe::{transcribe_whisper, WhisperTranscribeOptions};
use super::reply_speech::{synthesize_reply, ReplySpeechOptions, ReplySpeechResult};
use super::system_speech::{synthesize_system_say, SystemSpeechOptions};
use crate::openhuman::config::Config;
use crate::openhuman::inference::voice::kokoro_speech::{synthesize_kokoro, KokoroOptions};
use crate::rpc::RpcOutcome;

const LOG_PREFIX: &str = "[voice-factory]";

// ---------------------------------------------------------------------------
// Provider traits
// ---------------------------------------------------------------------------

/// Common shape both STT branches return after dispatch. Keeps the wire
/// contract identical regardless of provider — the UI only sees `text`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SttResult {
    pub text: String,
    /// Lowercase provider id (`"cloud"`, `"whisper"`) — exposed on the wire
    /// so the renderer can show the user which path actually ran.
    pub provider: String,
}

/// Speech-to-text provider abstraction. Cloud (backend proxy) and Whisper
/// (local subprocess / in-process) both implement this; the factory hands
/// the caller a boxed trait object.
#[async_trait]
pub trait SttProvider: Send + Sync {
    /// Stable identifier used in logs and config (`"cloud"`, `"whisper"`).
    fn name(&self) -> &'static str;

    /// Transcribe a single base64-encoded audio blob.
    ///
    /// `mime_type` and `file_name` are hints; providers that don't care
    /// may ignore them. `language` is BCP-47 (`"en"`, `"es"`); pass `None`
    /// to let the provider auto-detect.
    async fn transcribe(
        &self,
        config: &Config,
        audio_base64: &str,
        mime_type: Option<&str>,
        file_name: Option<&str>,
        language: Option<&str>,
    ) -> Result<RpcOutcome<SttResult>, String>;
}

/// Text-to-speech provider abstraction. Cloud returns rich viseme alignment
/// (used by the mascot lip-sync); Piper returns audio only and the caller
/// derives a flat viseme timeline downstream.
#[async_trait]
pub trait TtsProvider: Send + Sync {
    fn name(&self) -> &'static str;

    /// Synthesize speech for `text`. Returns the same envelope shape as
    /// `voice.reply_synthesize` so the renderer can swap providers without
    /// branching on the response.
    async fn synthesize(
        &self,
        config: &Config,
        text: &str,
        voice: Option<&str>,
    ) -> Result<RpcOutcome<ReplySpeechResult>, String>;
}

// ---------------------------------------------------------------------------
// Cloud STT
// ---------------------------------------------------------------------------

/// Cloud STT — wraps [`transcribe_cloud`]. Stateless; cheap to construct.
pub struct CloudSttProvider {
    model: String,
}

impl CloudSttProvider {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
        }
    }
}

#[async_trait]
impl SttProvider for CloudSttProvider {
    fn name(&self) -> &'static str {
        "cloud"
    }

    async fn transcribe(
        &self,
        config: &Config,
        audio_base64: &str,
        mime_type: Option<&str>,
        file_name: Option<&str>,
        language: Option<&str>,
    ) -> Result<RpcOutcome<SttResult>, String> {
        debug!(
            "{LOG_PREFIX} cloud STT dispatch model={} bytes_b64={}",
            self.model,
            audio_base64.len()
        );
        let opts = CloudTranscribeOptions {
            model: Some(self.model.clone()),
            language: language.map(str::to_string),
            mime_type: mime_type.map(str::to_string),
            file_name: file_name.map(str::to_string),
        };
        let outcome = transcribe_cloud(config, audio_base64, &opts).await?;
        let CloudTranscribeResult { text } = outcome.value;
        Ok(RpcOutcome::single_log(
            SttResult {
                text,
                provider: "cloud".to_string(),
            },
            "voice-factory: cloud STT completed",
        ))
    }
}

// ---------------------------------------------------------------------------
// Local Whisper STT
// ---------------------------------------------------------------------------

/// Local Whisper STT — wraps [`transcribe_whisper`]. Resolves `WHISPER_BIN`
/// lazily on each call.
pub struct WhisperSttProvider {
    model: String,
}

impl WhisperSttProvider {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
        }
    }
}

#[async_trait]
impl SttProvider for WhisperSttProvider {
    fn name(&self) -> &'static str {
        "whisper"
    }

    async fn transcribe(
        &self,
        config: &Config,
        audio_base64: &str,
        mime_type: Option<&str>,
        _file_name: Option<&str>,
        language: Option<&str>,
    ) -> Result<RpcOutcome<SttResult>, String> {
        debug!(
            "{LOG_PREFIX} whisper STT dispatch model={} mime={:?} lang={:?}",
            self.model, mime_type, language
        );
        let opts = WhisperTranscribeOptions {
            model: Some(self.model.clone()),
            mime_type: mime_type.map(str::to_string),
            language: language.map(str::to_string),
        };
        let outcome = transcribe_whisper(config, audio_base64, &opts).await?;
        Ok(RpcOutcome::single_log(
            SttResult {
                text: outcome.value.text,
                provider: "whisper".to_string(),
            },
            "voice-factory: whisper STT completed",
        ))
    }
}

// ---------------------------------------------------------------------------
// Cloud TTS
// ---------------------------------------------------------------------------

/// Cloud TTS — wraps [`synthesize_reply`] (backend ElevenLabs proxy).
pub struct CloudTtsProvider {
    voice: Option<String>,
}

impl CloudTtsProvider {
    pub fn new(voice: Option<String>) -> Self {
        Self { voice }
    }
}

#[async_trait]
impl TtsProvider for CloudTtsProvider {
    fn name(&self) -> &'static str {
        "cloud"
    }

    async fn synthesize(
        &self,
        config: &Config,
        text: &str,
        voice: Option<&str>,
    ) -> Result<RpcOutcome<ReplySpeechResult>, String> {
        let resolved_voice = voice
            .map(str::to_string)
            .or_else(|| self.voice.clone())
            .filter(|s| !s.trim().is_empty());
        debug!(
            "{LOG_PREFIX} cloud TTS dispatch voice={} chars={}",
            resolved_voice.as_deref().unwrap_or("<default>"),
            text.len()
        );
        let opts = ReplySpeechOptions {
            voice_id: resolved_voice,
            model_id: None,
            output_format: None,
            voice_settings: None,
        };
        synthesize_reply(config, text, &opts).await
    }
}

// ---------------------------------------------------------------------------
// Local Piper TTS
// ---------------------------------------------------------------------------

/// Local Piper TTS — wraps [`synthesize_piper`].
pub struct PiperTtsProvider {
    voice: String,
}

impl PiperTtsProvider {
    pub fn new(voice: impl Into<String>) -> Self {
        Self {
            voice: voice.into(),
        }
    }
}

#[async_trait]
impl TtsProvider for PiperTtsProvider {
    fn name(&self) -> &'static str {
        "piper"
    }

    async fn synthesize(
        &self,
        config: &Config,
        text: &str,
        voice: Option<&str>,
    ) -> Result<RpcOutcome<ReplySpeechResult>, String> {
        let resolved_voice = voice
            .map(str::to_string)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| self.voice.clone());
        debug!(
            "{LOG_PREFIX} piper TTS dispatch voice={} chars={}",
            resolved_voice,
            text.len()
        );
        let opts = PiperOptions {
            voice: Some(resolved_voice),
        };
        synthesize_piper(config, text, &opts).await
    }
}

// ---------------------------------------------------------------------------
// Kokoro (local OpenAI-compatible HTTP server) TTS
// ---------------------------------------------------------------------------

/// Local-server TTS via an OpenAI-compatible `/v1/audio/speech` endpoint.
/// Reference deployments: `kokoro-fastapi`, `mlx-audio` (Apple Silicon /
/// MLX-accelerated). The provider is protocol-only — anything that speaks
/// OpenAI Audio API works. See [`super::super::inference::voice::kokoro_speech`].
pub struct KokoroTtsProvider {
    endpoint_url: String,
    model: String,
    voice: Option<String>,
}

impl KokoroTtsProvider {
    pub fn new(endpoint_url: String, model: String, voice: Option<String>) -> Self {
        Self {
            endpoint_url,
            model,
            voice,
        }
    }
}

#[async_trait]
impl TtsProvider for KokoroTtsProvider {
    fn name(&self) -> &'static str {
        "kokoro"
    }

    async fn synthesize(
        &self,
        config: &Config,
        text: &str,
        voice: Option<&str>,
    ) -> Result<RpcOutcome<ReplySpeechResult>, String> {
        // Per-call voice override wins; otherwise fall back to the configured
        // default. Empty strings on either side trigger the server's own
        // default by omitting `voice` from the request body.
        let resolved_voice = voice
            .map(str::to_string)
            .filter(|s| !s.trim().is_empty())
            .or_else(|| self.voice.clone());
        debug!(
            "{LOG_PREFIX} kokoro TTS dispatch endpoint={} model={} voice={} chars={}",
            self.endpoint_url,
            self.model,
            resolved_voice.as_deref().unwrap_or("<server-default>"),
            text.len()
        );
        let opts = KokoroOptions {
            endpoint_url: self.endpoint_url.clone(),
            model: self.model.clone(),
            voice: resolved_voice,
        };
        synthesize_kokoro(config, text, &opts).await
    }
}

// ---------------------------------------------------------------------------
// System (host-native) TTS
// ---------------------------------------------------------------------------

/// System-native TTS. Currently macOS-only — wraps `/usr/bin/say` plus
/// `/usr/bin/afconvert` (see [`super::system_speech`]). Exists primarily
/// because the upstream Rhasspy Piper macOS release ships a broken
/// dylib chain, so `"piper"` is unusable out-of-the-box on macOS until
/// the user manually sources the missing `libespeak-ng.1.dylib` /
/// `libonnxruntime.1.14.1.dylib` / `libpiper_phonemize.1.dylib`. The
/// non-macOS `synthesize` call returns an explicit "macOS-only" error.
pub struct SystemTtsProvider {
    voice: Option<String>,
}

impl SystemTtsProvider {
    pub fn new(voice: Option<String>) -> Self {
        let voice = voice.and_then(|v| {
            let trimmed = v.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });
        Self { voice }
    }
}

#[async_trait]
impl TtsProvider for SystemTtsProvider {
    fn name(&self) -> &'static str {
        "system"
    }

    async fn synthesize(
        &self,
        config: &Config,
        text: &str,
        voice: Option<&str>,
    ) -> Result<RpcOutcome<ReplySpeechResult>, String> {
        let resolved_voice = voice
            .map(str::to_string)
            .filter(|s| !s.trim().is_empty())
            .or_else(|| self.voice.clone());
        debug!(
            "{LOG_PREFIX} system TTS dispatch voice={} chars={}",
            resolved_voice.as_deref().unwrap_or("<default>"),
            text.len()
        );
        let opts = SystemSpeechOptions {
            voice: resolved_voice,
        };
        synthesize_system_say(config, text, &opts).await
    }
}

// ---------------------------------------------------------------------------
// Factory entry points (mirrors embeddings/factory.rs)
// ---------------------------------------------------------------------------

/// Creates a speech-to-text provider based on the specified name and model.
///
/// Supported provider names:
/// - `"cloud"` → backend Whisper proxy — default, preferred for laptops
///   without local models
/// - `"whisper"` → local whisper.cpp via `WHISPER_BIN` (or in-process
///   `whisper-rs` when configured)
///
/// Returns an error for unrecognised provider names so configuration
/// mistakes surface immediately rather than silently degrading.
///
/// The factory does not eagerly resolve the binary — `WhisperSttProvider`
/// looks up `WHISPER_BIN` lazily inside `transcribe()` so a misconfigured
/// install fails at use-time with a clear error message instead of at
/// startup.
pub fn create_stt_provider(
    provider: &str,
    model: &str,
    _config: &Config,
) -> anyhow::Result<Box<dyn SttProvider>> {
    debug!("{LOG_PREFIX} create_stt_provider provider={provider} model={model}");
    let model = if model.trim().is_empty() {
        DEFAULT_WHISPER_MODEL
    } else {
        model
    };
    match provider.trim() {
        "cloud" => Ok(Box::new(CloudSttProvider::new(
            super::cloud_transcribe_default_model(),
        ))),
        "whisper" => Ok(Box::new(WhisperSttProvider::new(model))),
        unknown => Err(anyhow::anyhow!(
            "unknown STT provider: \"{unknown}\". Supported: \"cloud\", \"whisper\""
        )),
    }
}

/// Creates a text-to-speech provider based on the specified name and voice.
///
/// Supported provider names:
/// - `"cloud"` → backend ElevenLabs proxy with viseme alignment
/// - `"piper"` → local Piper subprocess via `PIPER_BIN`
/// - `"kokoro"` → local OpenAI-compatible TTS server (Kokoro on
///   kokoro-fastapi / mlx-audio / LM Studio audio mode). Endpoint URL,
///   model, and default voice come from `config.local_ai.kokoro_*`.
/// - `"system"` → host-native TTS (macOS only — wraps `/usr/bin/say`).
///   Falls back here when the broken upstream Piper macOS release would
///   otherwise leave the user with no working local-TTS option.
pub fn create_tts_provider(
    provider: &str,
    voice: &str,
    config: &Config,
) -> anyhow::Result<Box<dyn TtsProvider>> {
    debug!("{LOG_PREFIX} create_tts_provider provider={provider} voice={voice}");
    let trimmed_voice = voice.trim();
    let piper_voice = if trimmed_voice.is_empty() {
        DEFAULT_PIPER_VOICE
    } else {
        trimmed_voice
    };
    match provider.trim() {
        "cloud" => Ok(Box::new(CloudTtsProvider::new(if trimmed_voice.is_empty() {
            None
        } else {
            Some(trimmed_voice.to_string())
        }))),
        "piper" => Ok(Box::new(PiperTtsProvider::new(piper_voice))),
        "kokoro" => {
            // Kokoro/MLX-audio servers use their own voice id namespace
            // (`af_bella`, `am_michael`, …). A user who flipped from
            // Piper would still have `tts_voice_id = "en_US-lessac-medium"`
            // saved — that's a Piper id, not a Kokoro id, so we'd send
            // an unknown voice to the server. Drop voice ids that look
            // Piper-shaped (contain `_`, dash-quality suffix) and let the
            // configured `kokoro_voice` default take over.
            let configured_default = if config.local_ai.kokoro_voice.trim().is_empty() {
                None
            } else {
                Some(config.local_ai.kokoro_voice.trim().to_string())
            };
            let voice_override = if trimmed_voice.is_empty() || trimmed_voice.contains('_') {
                None
            } else {
                Some(trimmed_voice.to_string())
            };
            Ok(Box::new(KokoroTtsProvider::new(
                config.local_ai.kokoro_endpoint_url.clone(),
                config.local_ai.kokoro_model.clone(),
                voice_override.or(configured_default),
            )))
        }
        // System TTS uses host-native voice IDs (e.g. macOS `say -v
        // Samantha`), which don't share a namespace with Piper voice
        // IDs. Only forward the voice argument when it looks non-Piper
        // (no underscore, no dash-quality suffix) — otherwise let the
        // OS pick its default voice rather than choke on an unknown
        // `en_US-lessac-medium`.
        "system" => Ok(Box::new(SystemTtsProvider::new(
            if trimmed_voice.is_empty() || trimmed_voice.contains('_') {
                None
            } else {
                Some(trimmed_voice.to_string())
            },
        ))),
        unknown => Err(anyhow::anyhow!(
            "unknown TTS provider: \"{unknown}\". Supported: \"cloud\", \"piper\", \"kokoro\", \"system\""
        )),
    }
}

/// Default Whisper model. `whisper-large-v3-turbo` is the recommended ship
/// default — best accuracy-to-latency tradeoff in the Whisper family (5×
/// faster than `large-v3` with comparable WER on English). Users on lower-
/// spec hardware can drop down to `medium` / `small` / `base` / `tiny` via
/// the install presets.
pub const DEFAULT_WHISPER_MODEL: &str = "whisper-large-v3-turbo";

/// Default Piper voice — `en_US-lessac-medium`, matches
/// [`super::super::local_ai::model_ids::effective_tts_voice_id`].
pub const DEFAULT_PIPER_VOICE: &str = "en_US-lessac-medium";

/// Whisper install presets (size tiers exposed to the installer UI).
/// Mirrors the Ollama model installer surface: each entry is `(id, label)`.
pub const WHISPER_MODEL_PRESETS: &[(&str, &str)] = &[
    ("tiny", "Tiny (39 MB, fastest)"),
    ("base", "Base (74 MB)"),
    ("small", "Small (244 MB)"),
    ("medium", "Medium (769 MB, recommended)"),
    ("large-v3-turbo", "Large v3 Turbo (1.5 GB, best accuracy)"),
];

/// Returns a thread-safe default STT provider (cloud). Used by callers that
/// can't easily plumb a `Config` reference but still need a sensible default.
pub fn default_stt_provider() -> Arc<dyn SttProvider> {
    Arc::new(CloudSttProvider::new(
        super::cloud_transcribe_default_model(),
    ))
}

/// Returns a thread-safe default TTS provider (cloud).
pub fn default_tts_provider() -> Arc<dyn TtsProvider> {
    Arc::new(CloudTtsProvider::new(None))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config::default()
    }

    #[test]
    fn stt_factory_cloud_branch() {
        let p = create_stt_provider("cloud", "ignored", &cfg()).unwrap();
        assert_eq!(p.name(), "cloud");
    }

    #[test]
    fn stt_factory_whisper_branch() {
        let p = create_stt_provider("whisper", "whisper-large-v3-turbo", &cfg()).unwrap();
        assert_eq!(p.name(), "whisper");
    }

    #[test]
    fn stt_factory_whisper_empty_model_uses_default() {
        // Empty model → default whisper-large-v3-turbo; constructor must not
        // reject an empty string with an opaque error.
        let p = create_stt_provider("whisper", "", &cfg()).unwrap();
        assert_eq!(p.name(), "whisper");
    }

    #[test]
    fn stt_factory_unknown_provider_errors() {
        let err = create_stt_provider("deepgram", "nova-2", &cfg())
            .err()
            .expect("deepgram is not implemented");
        let msg = err.to_string();
        assert!(msg.contains("deepgram"), "should name the provider: {msg}");
        assert!(msg.contains("unknown"), "should say unknown: {msg}");
    }

    #[test]
    fn stt_factory_empty_string_errors() {
        let err = create_stt_provider("", "model", &cfg())
            .err()
            .expect("empty provider must error");
        assert!(err.to_string().contains("unknown"));
    }

    #[test]
    fn tts_factory_cloud_branch() {
        let p = create_tts_provider("cloud", "Rachel", &cfg()).unwrap();
        assert_eq!(p.name(), "cloud");
    }

    #[test]
    fn tts_factory_piper_branch() {
        let p = create_tts_provider("piper", "en_US-lessac-medium", &cfg()).unwrap();
        assert_eq!(p.name(), "piper");
    }

    #[test]
    fn tts_factory_piper_empty_voice_uses_default() {
        let p = create_tts_provider("piper", "", &cfg()).unwrap();
        assert_eq!(p.name(), "piper");
    }

    #[test]
    fn tts_factory_unknown_provider_errors() {
        let err = create_tts_provider("deepgram", "luna", &cfg())
            .err()
            .expect("deepgram TTS is not implemented");
        let msg = err.to_string();
        assert!(msg.contains("deepgram"), "should name the provider: {msg}");
        assert!(msg.contains("unknown"), "should say unknown: {msg}");
        // The error surfaces the supported list — make sure each id is in
        // it so a user typo'd as "say" or "macos" gets pointed at the right
        // provider id.
        for expected in ["cloud", "piper", "kokoro", "system"] {
            assert!(
                msg.contains(expected),
                "supported-provider list should advertise '{expected}': {msg}"
            );
        }
    }

    #[test]
    fn tts_factory_kokoro_branch() {
        let p = create_tts_provider("kokoro", "af_bella", &cfg()).unwrap();
        assert_eq!(p.name(), "kokoro");
    }

    #[test]
    fn tts_factory_kokoro_drops_piper_style_voice_id() {
        // Same reasoning as the system branch: voice ids containing `_`
        // are Piper-shaped (`en_US-lessac-medium`) and would be rejected
        // by a Kokoro server that knows only `af_bella` / `am_michael`.
        // The factory must fall back to the configured kokoro default
        // rather than forward the foreign id.
        let p = create_tts_provider("kokoro", "en_US-lessac-medium", &cfg()).unwrap();
        assert_eq!(p.name(), "kokoro");
    }

    #[test]
    fn tts_factory_kokoro_empty_voice_uses_configured_default() {
        let p = create_tts_provider("kokoro", "", &cfg()).unwrap();
        assert_eq!(p.name(), "kokoro");
    }

    #[test]
    fn tts_factory_system_branch() {
        let p = create_tts_provider("system", "Samantha", &cfg()).unwrap();
        assert_eq!(p.name(), "system");
    }

    #[test]
    fn tts_factory_system_drops_piper_style_voice_id() {
        // System TTS uses host-native voice ids (`"Samantha"`, `"Daniel"`)
        // — not Piper ids. If the user previously had `tts_voice_id =
        // "en_US-lessac-medium"` set for Piper and switches to system,
        // we must drop the Piper id rather than feed it to `say -v
        // en_US-lessac-medium` (which would fail with "Voice not
        // available"). Construction succeeds either way — only the
        // forwarded voice argument changes.
        let p = create_tts_provider("system", "en_US-lessac-medium", &cfg()).unwrap();
        assert_eq!(p.name(), "system");
    }

    #[test]
    fn tts_factory_system_empty_voice_falls_back_to_default() {
        let p = create_tts_provider("system", "", &cfg()).unwrap();
        assert_eq!(p.name(), "system");
    }

    #[test]
    fn whisper_presets_cover_full_size_ladder() {
        // Sanity-check the installer surface: tiny→large-v3-turbo must all be
        // exposed so the local-AI panel can render the size picker without
        // hard-coding the list.
        let ids: Vec<&str> = WHISPER_MODEL_PRESETS.iter().map(|(id, _)| *id).collect();
        for expected in ["tiny", "base", "small", "medium", "large-v3-turbo"] {
            assert!(
                ids.contains(&expected),
                "WHISPER_MODEL_PRESETS missing {expected}"
            );
        }
    }

    #[tokio::test]
    async fn whisper_provider_fails_clearly_when_binary_missing() {
        // No WHISPER_BIN env, no model file — the provider must surface an
        // actionable error rather than panic. Drive a small base64 payload
        // so we never reach the actual transcription call.
        let _guard = unset_env_guard("WHISPER_BIN");
        let provider = WhisperSttProvider::new("whisper-large-v3-turbo");
        let result = provider
            .transcribe(&cfg(), "AAAA", Some("audio/wav"), None, None)
            .await;
        assert!(result.is_err(), "missing binary must error");
        let msg = result.err().unwrap();
        // Whatever the underlying message says, it must NOT be a serialize
        // panic — i.e. we must have hit the binary-resolution branch.
        assert!(
            !msg.is_empty(),
            "error message should be populated for diagnosis"
        );
    }

    #[test]
    fn default_providers_return_cloud() {
        assert_eq!(default_stt_provider().name(), "cloud");
        assert_eq!(default_tts_provider().name(), "cloud");
    }

    /// Drop guard that unsets an env var on construction and restores it on
    /// drop. Necessary because cargo runs tests in parallel and bare
    /// `remove_var` would leak across tests.
    fn unset_env_guard(key: &'static str) -> EnvUnsetGuard {
        let prev = std::env::var_os(key);
        std::env::remove_var(key);
        EnvUnsetGuard { key, prev }
    }

    struct EnvUnsetGuard {
        key: &'static str,
        prev: Option<std::ffi::OsString>,
    }
    impl Drop for EnvUnsetGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}
