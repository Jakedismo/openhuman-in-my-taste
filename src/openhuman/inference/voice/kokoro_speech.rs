//! Local-server TTS via an OpenAI-compatible `/v1/audio/speech` endpoint.
//!
//! The reference target is **Kokoro** (82M-param neural TTS) served by
//! either [`kokoro-fastapi`] or [`mlx-audio`], both of which expose the
//! OpenAI Audio API shape:
//!
//! ```text
//! POST {endpoint}/v1/audio/speech
//! { "model": "kokoro", "input": "...", "voice": "af_bella",
//!   "response_format": "wav" }
//! → 200 OK, body = raw WAV bytes
//! ```
//!
//! The provider is intentionally protocol-only — any server that speaks
//! that shape works the same way (LM Studio's audio mode, mlx-audio,
//! kokoro-fastapi, custom wrappers). The user supplies the endpoint URL,
//! model, and default voice in Settings → Voice; nothing in this module
//! is Kokoro-specific.
//!
//! ## Why not embed an ONNX runtime in-process?
//!
//! Bundling onnxruntime + an espeak-ng phonemizer ships ~50 MB of
//! platform-specific native code per target and locks us into one
//! tokenizer/voice combo. Routing through HTTP lets the user pick the
//! best server for their hardware (MLX on Apple Silicon, CUDA on
//! NVIDIA, Metal/CoreML elsewhere) without rebuilding the desktop bundle.
//!
//! ## Log prefix
//!
//! `[voice-tts]` — matches the rest of the TTS pipeline so end-to-end
//! greps stay coherent.
//!
//! [`kokoro-fastapi`]: https://github.com/remsky/Kokoro-FastAPI
//! [`mlx-audio`]: https://github.com/Blaizzy/mlx-audio

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use futures::StreamExt;
use log::debug;
use serde_json::json;

use crate::openhuman::config::{build_runtime_proxy_client_with_timeouts, Config};
use crate::openhuman::voice::reply_speech::ReplySpeechResult;
use crate::rpc::RpcOutcome;

use super::local_speech::synthetic_viseme_timeline;

const LOG_PREFIX: &str = "[voice-tts]";

/// Caller-tunable knobs. Mirrors the shape of [`super::local_speech::PiperOptions`]
/// so the factory dispatch stays uniform.
#[derive(Debug, Default, Clone)]
pub struct KokoroOptions {
    /// Base URL (without `/v1/audio/speech`). Required; empty surfaces a
    /// clear configuration error rather than defaulting to an arbitrary
    /// port that may not be running.
    pub endpoint_url: String,
    /// Sent as `model` in the OpenAI body. Most servers ignore it but
    /// the field is part of the spec.
    pub model: String,
    /// Per-call voice override. When `None` or empty, fall back to the
    /// server's default — `voice` is omitted from the body entirely so
    /// the server picks for us.
    pub voice: Option<String>,
}

/// Synthesize via a local OpenAI-compatible TTS server.
///
/// Returns `ReplySpeechResult` with raw WAV bytes (base64-encoded) and a
/// synthetic flat viseme timeline. The mascot consumer accepts either rich
/// or synthetic timings — the alignment cost of getting accurate per-phoneme
/// timing from a third-party server isn't worth it for the mascot's coarse
/// mouth movements.
pub async fn synthesize_kokoro(
    config: &Config,
    text: &str,
    opts: &KokoroOptions,
) -> Result<RpcOutcome<ReplySpeechResult>, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("text is required".to_string());
    }

    let endpoint = opts.endpoint_url.trim();
    if endpoint.is_empty() {
        return Err(format!(
            "{LOG_PREFIX} kokoro endpoint URL not configured \
             (Settings → Voice → Kokoro endpoint)"
        ));
    }

    let model = opts.model.trim();
    let model = if model.is_empty() { "kokoro" } else { model };

    // Build the URL by appending `/v1/audio/speech` to whatever base the
    // user gave us. Strip trailing slashes so we don't end up with a
    // double-slash that some servers reject (kokoro-fastapi tolerates it
    // but mlx-audio is stricter).
    let base = endpoint.trim_end_matches('/');
    let url = format!("{base}/v1/audio/speech");

    let mut body = serde_json::Map::new();
    body.insert("model".to_string(), json!(model));
    body.insert("input".to_string(), json!(trimmed));
    body.insert("response_format".to_string(), json!("wav"));
    if let Some(v) = opts
        .voice
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        body.insert("voice".to_string(), json!(v));
    }

    // The runtime proxy registry keys by service name, not by Config — the
    // `config` borrow is kept on the signature for symmetry with the other
    // factory branches. 120s/10s timeouts cover multi-paragraph synth on CPU
    // servers while still failing fast on a misconfigured host.
    let _ = config;
    let client = build_runtime_proxy_client_with_timeouts("kokoro_tts", 120, 10);

    debug!(
        "{LOG_PREFIX} kokoro POST url={} model={} voice={} chars={}",
        url,
        model,
        opts.voice.as_deref().unwrap_or("<default>"),
        trimmed.len()
    );

    let started = std::time::Instant::now();
    let response = client
        .post(&url)
        // mlx-audio (and several Kokoro wrappers) content-negotiate
        // differently when no `Accept` is set — explicit `audio/wav`
        // prevents them from defaulting to a streaming MP3 path that
        // confuses downstream WAV consumers.
        .header(reqwest::header::ACCEPT, "audio/wav")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("{LOG_PREFIX} kokoro POST failed ({url}): {e}"))?;

    let status = response.status();
    if !status.is_success() {
        let detail = response
            .text()
            .await
            .unwrap_or_else(|_| "<no body>".to_string());
        return Err(format!(
            "{LOG_PREFIX} kokoro server returned {status} ({url}): {}",
            detail.chars().take(400).collect::<String>()
        ));
    }

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("<missing>")
        .to_string();
    let content_length = response
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());

    // Read the body via a streaming accumulator instead of `.bytes()`.
    // mlx-audio's `--realtime-model` flag puts /v1/audio/speech into a
    // chunked streaming mode (WAV header + streamed PCM frames). `.bytes()`
    // surfaces a single opaque "error decoding response body" if the
    // transfer-encoded stream ends abnormally — the accumulator lets us
    // report how many bytes we received before the break, so we can tell
    // the server crashed mid-synth from "server never responded" cases.
    let mut audio_buf: Vec<u8> = match content_length {
        Some(n) => Vec::with_capacity(n.min(64 * 1024 * 1024) as usize),
        None => Vec::with_capacity(256 * 1024),
    };
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(bytes) => audio_buf.extend_from_slice(&bytes),
            Err(e) => {
                return Err(format!(
                    "{LOG_PREFIX} kokoro response stream broke after {} bytes \
                     (content-type={}, content-length={:?}): {e}",
                    audio_buf.len(),
                    content_type,
                    content_length
                ));
            }
        }
    }
    if audio_buf.is_empty() {
        return Err(format!(
            "{LOG_PREFIX} kokoro returned an empty audio body \
             (content-type={content_type}) — server is misconfigured"
        ));
    }
    // Sanity-check the magic: a real WAV starts `RIFF…WAVE`. If the
    // server handed us something else (a streamed MP3, raw PCM without
    // a header, or an error blob masquerading as audio), surface that
    // here so the user knows to flip `response_format` or drop
    // `--realtime-model`.
    let looks_like_wav =
        audio_buf.len() >= 12 && &audio_buf[0..4] == b"RIFF" && &audio_buf[8..12] == b"WAVE";
    if !looks_like_wav {
        let preview = audio_buf
            .iter()
            .take(16)
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        debug!(
            "{LOG_PREFIX} kokoro response did NOT start with RIFF/WAVE magic \
             (content-type={content_type}, first 16 bytes: {preview}) — \
             passing through anyway",
        );
    }

    let audio_bytes = audio_buf;
    let audio_base64 = BASE64.encode(&audio_bytes);
    let visemes = synthetic_viseme_timeline(trimmed);
    debug!(
        "{LOG_PREFIX} kokoro synthesized wav_bytes={} content_type={} \
         is_riff_wave={} visemes={} elapsed_ms={}",
        audio_bytes.len(),
        content_type,
        looks_like_wav,
        visemes.len(),
        started.elapsed().as_millis()
    );

    Ok(RpcOutcome::single_log(
        ReplySpeechResult {
            audio_base64,
            audio_mime: "audio/wav".to_string(),
            visemes,
            alignment: None,
        },
        "kokoro local TTS completed",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_empty_text() {
        let config = Config::default();
        let opts = KokoroOptions {
            endpoint_url: "http://localhost:8880".to_string(),
            model: "kokoro".to_string(),
            voice: None,
        };
        let err = synthesize_kokoro(&config, "", &opts)
            .await
            .err()
            .expect("empty text must error");
        assert!(err.contains("required"), "empty text error mismatch: {err}");
    }

    #[tokio::test]
    async fn rejects_whitespace_only_text() {
        let config = Config::default();
        let opts = KokoroOptions {
            endpoint_url: "http://localhost:8880".to_string(),
            model: "kokoro".to_string(),
            voice: None,
        };
        let err = synthesize_kokoro(&config, "   \n\t ", &opts)
            .await
            .err()
            .expect("whitespace text must error");
        assert!(err.contains("required"), "whitespace error mismatch: {err}");
    }

    #[tokio::test]
    async fn rejects_empty_endpoint() {
        let config = Config::default();
        let opts = KokoroOptions {
            endpoint_url: "".to_string(),
            model: "kokoro".to_string(),
            voice: None,
        };
        let err = synthesize_kokoro(&config, "hello", &opts)
            .await
            .err()
            .expect("empty endpoint must error");
        assert!(
            err.contains("endpoint"),
            "missing-endpoint error mismatch: {err}"
        );
    }

    #[tokio::test]
    async fn unreachable_endpoint_surfaces_clear_error() {
        // Port 1 is reserved and never listens; the POST must surface a
        // network error rather than panic in the spawn path.
        let config = Config::default();
        let opts = KokoroOptions {
            endpoint_url: "http://127.0.0.1:1".to_string(),
            model: "kokoro".to_string(),
            voice: Some("af_bella".to_string()),
        };
        let err = synthesize_kokoro(&config, "hello world", &opts)
            .await
            .err()
            .expect("unreachable endpoint must error");
        assert!(
            err.contains("kokoro POST failed") || err.contains("connect"),
            "should surface network error: {err}"
        );
    }
}
