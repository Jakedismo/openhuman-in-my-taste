//! Local text-to-speech via the host operating system's built-in
//! speech synthesizer. On **macOS** this shells out to `/usr/bin/say`
//! and pipes the resulting AIFF through `/usr/bin/afconvert` to produce
//! a 16-bit PCM WAV that matches the mime-type contract every other
//! TTS provider in this crate emits (`audio/wav`).
//!
//! ## Why this exists (and why it's macOS-only)
//!
//! The Rhasspy Piper `2023.11.14-2` release — the only macOS release —
//! ships a binary that depends on `@rpath/libespeak-ng.1.dylib`,
//! `@rpath/libpiper_phonemize.1.dylib`, and `@rpath/libonnxruntime.1.14.1.dylib`,
//! but the tarball contains **none of those dylibs** and the binary
//! has **no `LC_RPATH` entries** to find them. Net effect: Piper TTS is
//! permanently broken out-of-the-box on macOS, with no upstream fix in
//! sight. The macOS-native `say(1)` command is bundled with every
//! supported macOS version, requires no install, and produces decent
//! audio for a mascot voice preview, so we route through that on the
//! Apple platform instead.
//!
//! Linux and Windows users keep `"piper"` (works) or `"cloud"`
//! (always works) as their TTS provider — this module's `synthesize`
//! returns a clear "system TTS provider is macOS-only" error when
//! invoked on any other platform so a misconfigured non-macOS
//! installation fails loudly instead of silently producing empty audio.
//!
//! ## Log prefix
//!
//! `[voice-tts]` — matches the rest of the TTS pipeline so end-to-end
//! greps stay coherent.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};

use crate::openhuman::config::Config;
use crate::openhuman::voice::reply_speech::ReplySpeechResult;
use crate::rpc::RpcOutcome;

use super::local_speech::synthetic_viseme_timeline;

const LOG_PREFIX: &str = "[voice-tts]";

/// Caller-tunable knobs for system-native synthesis.
///
/// Mirrors [`super::local_speech::PiperOptions`] in spirit so the
/// factory dispatch stays uniform. The `voice` field is currently
/// unused on macOS because `say` voice ids (e.g. `"Samantha"`,
/// `"Daniel"`) don't share a namespace with Piper voice ids
/// (`"en_US-lessac-medium"`); rather than maintain a hand-rolled
/// translation table we let `say` pick its system-default voice. A
/// future iteration can add a macOS voice picker that round-trips the
/// `say -v ?` list.
#[derive(Debug, Default, Clone)]
pub struct SystemSpeechOptions {
    /// Reserved for future macOS voice selection (e.g. `"Samantha"`).
    /// Currently ignored — `say` uses the system default.
    pub voice: Option<String>,
}

/// Synthesize speech with the host OS's built-in TTS.
///
/// **macOS** is the only supported platform — see the module header for
/// the reasoning. Calling this on Linux/Windows surfaces an explicit
/// error so a misconfigured `tts_provider = "system"` fails the voice
/// preview with an actionable message instead of returning silently.
pub async fn synthesize_system_say(
    _config: &Config,
    text: &str,
    opts: &SystemSpeechOptions,
) -> Result<RpcOutcome<ReplySpeechResult>, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("text is required".to_string());
    }

    #[cfg(target_os = "macos")]
    {
        synthesize_macos(trimmed, opts).await
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = opts;
        Err(format!(
            "{LOG_PREFIX} system TTS provider is macOS-only \
             (no /usr/bin/say equivalent on this OS). \
             Switch tts_provider to 'cloud' or 'piper'."
        ))
    }
}

#[cfg(target_os = "macos")]
async fn synthesize_macos(
    text: &str,
    opts: &SystemSpeechOptions,
) -> Result<RpcOutcome<ReplySpeechResult>, String> {
    use log::debug;

    let work_dir = std::env::temp_dir().join("openhuman_voice_output");
    tokio::fs::create_dir_all(&work_dir)
        .await
        .map_err(|e| format!("{LOG_PREFIX} failed to create voice output directory: {e}"))?;

    let stamp = chrono::Utc::now().timestamp_millis();
    let uuid = uuid::Uuid::new_v4();
    let aiff_path = work_dir.join(format!("say-{stamp}-{uuid}.aiff"));
    let wav_path = work_dir.join(format!("say-{stamp}-{uuid}.wav"));

    // ── Stage 1: say -> AIFF ────────────────────────────────────────────
    //
    // We pipe the text to stdin (`--input-file=-`) instead of passing it
    // as an argv positional. argv would require escaping shell metachars
    // and bumps into ARG_MAX for long inputs; stdin avoids both issues.
    let spawn_started = std::time::Instant::now();
    let mut say_cmd = tokio::process::Command::new("/usr/bin/say");
    say_cmd.arg("--input-file=-");
    if let Some(voice) = opts
        .voice
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        say_cmd.args(["-v", voice]);
    }
    say_cmd.args(["-o", &aiff_path.to_string_lossy()]);
    say_cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());

    let mut say_child = say_cmd
        .spawn()
        .map_err(|e| format!("{LOG_PREFIX} failed to launch /usr/bin/say: {e}"))?;

    if let Some(mut stdin) = say_child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        stdin
            .write_all(text.as_bytes())
            .await
            .map_err(|e| format!("{LOG_PREFIX} failed to write text to say stdin: {e}"))?;
    }

    let say_output = say_child
        .wait_with_output()
        .await
        .map_err(|e| format!("{LOG_PREFIX} failed to wait on /usr/bin/say: {e}"))?;
    if !say_output.status.success() {
        let _ = tokio::fs::remove_file(&aiff_path).await;
        return Err(format!(
            "{LOG_PREFIX} /usr/bin/say failed (exit={:?}): {}",
            say_output.status.code(),
            String::from_utf8_lossy(&say_output.stderr).trim()
        ));
    }
    debug!(
        "{LOG_PREFIX} say wrote {} ({} ms)",
        aiff_path.display(),
        spawn_started.elapsed().as_millis()
    );

    // ── Stage 2: afconvert AIFF -> 16-bit PCM WAV ───────────────────────
    //
    // The renderer expects `audio/wav` — every other TTS provider in this
    // crate emits WAV, and the mascot's audio playback path doesn't know
    // about AIFF. `afconvert` is built into macOS (CoreAudio toolchain)
    // and the `-f WAVE -d LEI16` combination is the simplest "just give
    // me a normal PCM WAV" invocation.
    let convert_started = std::time::Instant::now();
    let convert_output = tokio::process::Command::new("/usr/bin/afconvert")
        .args([
            "-f",
            "WAVE",
            "-d",
            "LEI16",
            &aiff_path.to_string_lossy(),
            &wav_path.to_string_lossy(),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("{LOG_PREFIX} failed to launch /usr/bin/afconvert: {e}"))?;

    // AIFF is no longer needed regardless of conversion outcome.
    let _ = tokio::fs::remove_file(&aiff_path).await;

    if !convert_output.status.success() {
        let _ = tokio::fs::remove_file(&wav_path).await;
        return Err(format!(
            "{LOG_PREFIX} /usr/bin/afconvert failed (exit={:?}): {}",
            convert_output.status.code(),
            String::from_utf8_lossy(&convert_output.stderr).trim()
        ));
    }
    debug!(
        "{LOG_PREFIX} afconvert wrote {} ({} ms)",
        wav_path.display(),
        convert_started.elapsed().as_millis()
    );

    // ── Stage 3: read WAV, base64-encode, clean up, return ──────────────
    let audio_bytes = tokio::fs::read(&wav_path)
        .await
        .map_err(|e| format!("{LOG_PREFIX} failed to read afconvert output: {e}"))?;
    if let Err(e) = tokio::fs::remove_file(&wav_path).await {
        log::warn!(
            "{LOG_PREFIX} failed to clean up say output {}: {e}",
            wav_path.display()
        );
    }

    let audio_base64 = BASE64.encode(&audio_bytes);
    let visemes = synthetic_viseme_timeline(text);
    debug!(
        "{LOG_PREFIX} synthesized via /usr/bin/say wav_bytes={} visemes={}",
        audio_bytes.len(),
        visemes.len()
    );

    Ok(RpcOutcome::single_log(
        ReplySpeechResult {
            audio_base64,
            audio_mime: "audio/wav".to_string(),
            visemes,
            alignment: None,
        },
        "macOS system TTS (say + afconvert) completed",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn synthesize_system_say_rejects_empty_text() {
        let config = Config::default();
        let opts = SystemSpeechOptions::default();
        let err = synthesize_system_say(&config, "", &opts)
            .await
            .err()
            .expect("empty text must error");
        assert!(err.contains("required"), "empty text error mismatch: {err}");
    }

    #[tokio::test]
    async fn synthesize_system_say_rejects_whitespace_only_text() {
        let config = Config::default();
        let opts = SystemSpeechOptions::default();
        let err = synthesize_system_say(&config, "   \t\n   ", &opts)
            .await
            .err()
            .expect("whitespace text must error");
        assert!(
            err.contains("required"),
            "whitespace text error mismatch: {err}"
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[tokio::test]
    async fn synthesize_system_say_errors_on_non_macos() {
        let config = Config::default();
        let opts = SystemSpeechOptions::default();
        let err = synthesize_system_say(&config, "hello there", &opts)
            .await
            .err()
            .expect("non-macOS must surface the macOS-only error");
        assert!(
            err.contains("macOS-only"),
            "non-macOS branch should mention macOS-only: {err}"
        );
    }

    // macOS happy-path is intentionally NOT tested inline. The end-to-end
    // /usr/bin/say + /usr/bin/afconvert pipeline depends on the host
    // audio toolchain and writes ~tens of KB to disk; gating it on the
    // existence of those binaries would gate it on the local box's exact
    // OS image. Coverage for that case lives in the manual smoke test
    // documented in the mascot panel.
}
