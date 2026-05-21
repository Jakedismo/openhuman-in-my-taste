//! Workload-routed chat provider — adapts the unified
//! [`crate::openhuman::inference::provider::factory`] (`provider_for_role`
//! + `create_chat_provider_from_string`) to the memory-tree
//! [`super::ChatProvider`] trait surface.
//!
//! Used when the user has configured a non-Ollama cloud provider for the
//! `memory` workload (e.g. an `openai:gpt-5.4-mini` row in
//! `cloud_providers`). This is the only cloud-side memory-tree provider —
//! the legacy `CloudChatProvider` (which wrapped the dead OpenHuman
//! backend) was removed when the local-OAuth fork stopped shipping a
//! product backend.
//!
//! Built once per `build_chat_provider` call. The underlying
//! `Box<dyn Provider>` from the factory is `Send + Sync`, so the
//! adapter is safe to wrap in an `Arc<dyn ChatProvider>`.

use anyhow::{Context, Result};
use async_trait::async_trait;

use crate::openhuman::inference::provider::traits::Provider;

use super::{ChatPrompt, ChatProvider};

/// Adapter for a workload-factory-built [`Provider`] (OpenAI-compatible,
/// Anthropic, etc.) plugged into the memory-tree chat surface.
pub struct WorkloadChatProvider {
    inner: Box<dyn Provider>,
    model: String,
    /// Cached display name `"<slug>:<model>"` or `"cloud-workload:<model>"`.
    display: String,
}

impl WorkloadChatProvider {
    /// Wrap a `(Provider, model)` tuple produced by the workload factory.
    ///
    /// `slug_hint` is a short label included in the provider display name
    /// for log greppability (e.g. `"openai"` from the cloud_providers
    /// slug, or `"workload"` when the caller doesn't know the slug).
    pub fn new(inner: Box<dyn Provider>, model: String, slug_hint: &str) -> Self {
        let trimmed = slug_hint.trim();
        let label = if trimmed.is_empty() {
            "workload"
        } else {
            trimmed
        };
        let display = format!("{label}:{model}");
        Self {
            inner,
            model,
            display,
        }
    }
}

#[async_trait]
impl ChatProvider for WorkloadChatProvider {
    fn name(&self) -> &str {
        &self.display
    }

    async fn chat_for_json(&self, prompt: &ChatPrompt) -> Result<String> {
        log::debug!(
            "[memory_tree::chat::workload] kind={} model={} sys_chars={} user_chars={}",
            prompt.kind,
            self.model,
            prompt.system.len(),
            prompt.user.len()
        );

        let response = self
            .inner
            .chat_with_system(
                Some(prompt.system.as_str()),
                prompt.user.as_str(),
                self.model.as_str(),
                prompt.temperature,
            )
            .await
            .with_context(|| {
                format!(
                    "workload chat request kind={} model={} failed",
                    prompt.kind, self.model
                )
            })?;

        log::debug!(
            "[memory_tree::chat::workload] response chars={} kind={}",
            response.len(),
            prompt.kind
        );
        Ok(response)
    }
}
