//! Resolved model / voice IDs from [`crate::openhuman::config::Config`].
//!
//! In the local-OAuth single-user fork there is no managed MVP tier —
//! the user owns the Ollama / LM Studio runtime and picks their own
//! models. The historical "MVP allowlist" that silently rewrote
//! user-chosen model IDs to a tiny pre-approved set has been
//! converted to pass-through with a warning, so any model the user
//! has actually pulled flows through unchanged. The embedding helper
//! still logs a stronger warning if the chosen model isn't a known
//! 1024-dim embedder, because the memory tree's on-disk format is
//! fixed to that dimensionality and a mismatch corrupts the store —
//! but the user's choice is respected; we trust they know their
//! model dims when they override.

use crate::openhuman::config::Config;
use crate::openhuman::inference::local::provider::{provider_from_config, LocalAiProvider};

pub(crate) const DEFAULT_OLLAMA_MODEL: &str = "gemma3:1b-it-qat";
pub(crate) const DEFAULT_OLLAMA_VISION_MODEL: &str = "";
pub(crate) const DEFAULT_OLLAMA_EMBED_MODEL: &str = "bge-m3";

/// Embedding models known to match the 1024-dim contract the memory
/// tree's on-disk format was built for. Selections outside this set
/// are PERMITTED but a warning is logged so the user has a chance
/// to notice before memory writes start failing on dim mismatch.
///
/// The Qwen3 embedding family (`qwen3-embedding:0.6b`, `:4b`, `:8b`)
/// is included because every published Qwen3 embedder ships at 1024
/// hidden dim — matching the memory tree's on-disk format. Adding
/// them here suppresses the spurious dim-warning that fires on every
/// memory ingest when the user has selected a Qwen3 embedder.
const KNOWN_COMPATIBLE_EMBEDDING_MODELS: &[&str] = &[
    "bge-m3",
    "all-minilm:latest",
    "qwen3-embedding:0.6b",
    "qwen3-embedding:4b",
    "qwen3-embedding:8b",
];

fn enforce_mvp_chat_allowlist(resolved: &str) -> String {
    // Local-OAuth fork: trust the user's model selection. The legacy
    // allowlist was an MVP-build artefact that doesn't apply when the
    // operator manages their own Ollama instance.
    resolved.to_string()
}

fn enforce_mvp_vision_allowlist(resolved: &str) -> String {
    // Same as chat — pass through whatever vision model the user
    // configured. If their Ollama doesn't have it, the request will
    // surface a real "model not found" error on first use, which is
    // more useful than silently disabling vision.
    resolved.to_string()
}

fn enforce_mvp_embedding_allowlist(resolved: &str) -> String {
    let lower = resolved.to_ascii_lowercase();
    if KNOWN_COMPATIBLE_EMBEDDING_MODELS
        .iter()
        .any(|m| lower == m.to_ascii_lowercase())
    {
        return resolved.to_string();
    }
    // Pass the user's choice through, but warn — memory tree files
    // are fixed to 1024-dim and a mismatched embedder will corrupt
    // writes. If the user has chosen e.g. `qwen3-embedding:8b` they
    // need to confirm it's 1024-dim before relying on it.
    tracing::warn!(
        resolved,
        known_safe = ?KNOWN_COMPATIBLE_EMBEDDING_MODELS,
        "[local_ai] embedding model is outside the known-compatible 1024-dim set; \
         passing through, but memory writes will fail if the model's vector \
         dimension differs from the on-disk format"
    );
    resolved.to_string()
}

pub(crate) fn effective_chat_model_id(config: &Config) -> String {
    let provider = provider_from_config(config);
    if provider == LocalAiProvider::LmStudio {
        let model_id = raw_chat_model_id(config);
        tracing::debug!(
            provider = provider.as_str(),
            has_model = !model_id.is_empty(),
            "[local_ai] effective_chat_model_id: using provider-managed model id"
        );
        return model_id;
    }

    let raw = if !config.local_ai.chat_model_id.trim().is_empty() {
        config.local_ai.chat_model_id.trim()
    } else {
        config.local_ai.model_id.trim()
    };
    if raw.is_empty() {
        return enforce_mvp_chat_allowlist(DEFAULT_OLLAMA_MODEL);
    }
    // Local-OAuth fork: trust the user. Older builds rewrote specific
    // legacy model IDs (qwen3-1.7b, qwen2.5-1.5b-instruct, anything
    // ending in .gguf, anything mentioning huggingface.co/) to the
    // MVP default so unsupported assets were silently downgraded.
    // That made sense when the app shipped a single bundled model;
    // here the user manages Ollama themselves and we should never
    // silently swap their selection.
    enforce_mvp_chat_allowlist(raw)
}

fn raw_chat_model_id(config: &Config) -> String {
    // For LM Studio the user must set `local_ai.chat_model_id` explicitly —
    // there is no sensible Ollama-branded default to fall back to. Return an
    // empty string so callers (diagnostics, status) surface the missing-model
    // warning rather than silently requesting "gemma3:1b-it-qat" from LM Studio.
    let raw = if !config.local_ai.chat_model_id.trim().is_empty() {
        config.local_ai.chat_model_id.trim()
    } else {
        config.local_ai.model_id.trim()
    };
    if raw.is_empty() {
        tracing::debug!(
            provider = "lm_studio",
            "[local_ai] raw_chat_model_id: no LM Studio chat model configured"
        );
    }
    raw.to_string()
}

pub(crate) fn effective_vision_model_id(config: &Config) -> String {
    let raw = config.local_ai.vision_model_id.trim();
    if raw.is_empty() {
        return String::new();
    }
    // Local-OAuth fork: trust the user. Older builds rewrote bare
    // `moondream` / `moondream:1.8b` to the specific quantised
    // variant `moondream:1.8b-v2-q4_K_S` because the bundled
    // installer pulled that exact tag. The user's Ollama now has
    // whatever they pulled — pass it through unchanged.
    enforce_mvp_vision_allowlist(raw)
}

pub(crate) fn effective_embedding_model_id(config: &Config) -> String {
    let raw = config.local_ai.embedding_model_id.trim();
    if raw.is_empty() {
        return enforce_mvp_embedding_allowlist(DEFAULT_OLLAMA_EMBED_MODEL);
    }
    enforce_mvp_embedding_allowlist(raw)
}

pub(crate) fn effective_stt_model_id(config: &Config) -> String {
    let raw = config.local_ai.stt_model_id.trim();
    if raw.is_empty() {
        "ggml-base-q5_1.bin".to_string()
    } else {
        raw.to_string()
    }
}

pub(crate) fn effective_tts_voice_id(config: &Config) -> String {
    let raw = config.local_ai.tts_voice_id.trim();
    if raw.is_empty() {
        "en_US-lessac-medium".to_string()
    } else {
        raw.to_string()
    }
}

pub(crate) fn effective_quantization(config: &Config) -> String {
    let raw = config.local_ai.quantization.trim();
    if raw.is_empty() {
        "q4".to_string()
    } else {
        raw.to_ascii_lowercase()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> Config {
        Config::default()
    }

    #[test]
    fn chat_model_falls_back_to_ollama_default_when_empty() {
        let mut config = test_config();

        config.local_ai.chat_model_id = String::new();
        config.local_ai.model_id = String::new();
        // Empty input always falls back to the Ollama default — the
        // user gave us nothing to pass through.
        assert_eq!(effective_chat_model_id(&config), DEFAULT_OLLAMA_MODEL);
    }

    #[test]
    fn chat_model_passes_through_user_selection() {
        // Local-OAuth fork: trust user's chat model selection.
        let mut config = test_config();
        config.local_ai.chat_model_id = "gemma3:1b-it-qat".to_string();
        assert_eq!(effective_chat_model_id(&config), "gemma3:1b-it-qat");

        config.local_ai.chat_model_id = "qwen3-1.7b".to_string();
        assert_eq!(effective_chat_model_id(&config), "qwen3-1.7b");

        config.local_ai.chat_model_id = "gemma4:e4b".to_string();
        assert_eq!(effective_chat_model_id(&config), "gemma4:e4b");
    }

    #[test]
    fn chat_model_allows_custom_ids_for_lm_studio() {
        let mut config = test_config();
        config.local_ai.provider = "lm_studio".to_string();
        config.local_ai.chat_model_id = "publisher/custom-model-7b".to_string();
        assert_eq!(
            effective_chat_model_id(&config),
            "publisher/custom-model-7b"
        );
    }

    #[test]
    fn lm_studio_chat_model_returns_empty_when_no_model_configured() {
        // LM Studio has no sensible Ollama-branded default — an empty model ID
        // surfaces the missing-model warning in diagnostics / status rather than
        // silently sending "gemma3:1b-it-qat" to an LM Studio server.
        let mut config = test_config();
        config.local_ai.provider = "lm_studio".to_string();
        config.local_ai.chat_model_id = String::new();
        config.local_ai.model_id = String::new();
        assert_eq!(effective_chat_model_id(&config), "");
    }

    #[test]
    fn vision_model_passes_through_user_selection() {
        // Local-OAuth fork: trust user's vision model selection. The
        // legacy MVP allowlist that silently disabled non-empty
        // vision models was an artefact of the managed-MVP build.
        let mut config = test_config();
        config.local_ai.vision_model_id = String::new();
        assert_eq!(effective_vision_model_id(&config), "");

        config.local_ai.vision_model_id = "moondream".to_string();
        assert_eq!(effective_vision_model_id(&config), "moondream");
        config.local_ai.vision_model_id = "moondream:1.8b".to_string();
        assert_eq!(effective_vision_model_id(&config), "moondream:1.8b");
    }

    #[test]
    fn embedding_model_empty_falls_back_to_bge_m3() {
        // After the cloud-embeddings unification PR, the default embedder
        // for the local Ollama path is bge-m3 (1024 dim) to match memory
        // tree's fixed on-disk format. Empty / whitespace input must
        // resolve to that default, not the prior all-minilm:latest.
        let mut config = test_config();
        config.local_ai.embedding_model_id = String::new();
        assert_eq!(effective_embedding_model_id(&config), "bge-m3");

        config.local_ai.embedding_model_id = "   ".to_string();
        assert_eq!(effective_embedding_model_id(&config), "bge-m3");
    }

    #[test]
    fn embedding_model_passes_through_known_compatible_values() {
        // all-minilm:latest is in KNOWN_COMPATIBLE_EMBEDDING_MODELS
        // for back-compat with users who already pulled it.
        let mut config = test_config();
        config.local_ai.embedding_model_id = "all-minilm:latest".to_string();
        assert_eq!(effective_embedding_model_id(&config), "all-minilm:latest");
    }

    #[test]
    fn embedding_model_passes_user_selection_outside_known_set_with_warning() {
        // Local-OAuth fork: trust the user's choice but log a warning
        // (the memory tree's on-disk format is 1024-dim; mismatched
        // embedders will surface a dim error at embed time). The
        // value itself is passed through unchanged so the user can
        // intentionally select e.g. qwen3-embedding:8b if they've
        // confirmed dim compatibility.
        let mut config = test_config();
        config.local_ai.embedding_model_id = "qwen3-embedding:8b".to_string();
        assert_eq!(effective_embedding_model_id(&config), "qwen3-embedding:8b");

        config.local_ai.embedding_model_id = "totally-made-up-model:v0".to_string();
        assert_eq!(
            effective_embedding_model_id(&config),
            "totally-made-up-model:v0"
        );
    }

    #[test]
    fn stt_tts_and_quantization_defaults_are_applied() {
        let mut config = test_config();
        config.local_ai.stt_model_id.clear();
        config.local_ai.tts_voice_id.clear();
        config.local_ai.quantization = "Q5_K_M".to_string();

        assert_eq!(effective_stt_model_id(&config), "ggml-base-q5_1.bin");
        assert_eq!(effective_tts_voice_id(&config), "en_US-lessac-medium");
        assert_eq!(effective_quantization(&config), "q5_k_m");
    }
}
