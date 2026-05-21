//! RPC-facing operations for the vault domain.

use chrono::Utc;
use uuid::Uuid;

use crate::openhuman::config::Config;
use crate::openhuman::memory::ops::{clear_namespace, ClearNamespaceParams};
use crate::rpc::RpcOutcome;

use super::jobs;
use super::store;
use super::types::{Vault, VaultFile, VaultSyncJobHandle, VaultSyncJobSnapshot};

/// Create a new vault pointing at a local folder.
pub async fn vault_create(
    config: &Config,
    name: &str,
    root_path: &str,
    include_globs: Vec<String>,
    exclude_globs: Vec<String>,
) -> Result<RpcOutcome<Vault>, String> {
    let trimmed_name = name.trim();
    if trimmed_name.is_empty() {
        return Err("vault name must not be empty".to_string());
    }
    let trimmed_root = root_path.trim();
    if trimmed_root.is_empty() {
        return Err("root_path must not be empty".to_string());
    }
    let root = std::path::Path::new(trimmed_root);
    if !root.is_absolute() {
        return Err(format!("root_path must be absolute: {trimmed_root}"));
    }
    if !root.is_dir() {
        return Err(format!("root_path is not a directory: {trimmed_root}"));
    }

    let id = Uuid::new_v4().to_string();
    log::debug!(
        "[vault] create: name={trimmed_name:?} root={trimmed_root:?} id={id} \
         include_globs={} exclude_globs={}",
        include_globs.len(),
        exclude_globs.len(),
    );
    let namespace = format!("vault:{id}");
    let vault = Vault {
        id: id.clone(),
        name: trimmed_name.to_string(),
        root_path: root
            .canonicalize()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| trimmed_root.to_string()),
        namespace,
        include_globs,
        exclude_globs,
        created_at: Utc::now(),
        last_synced_at: None,
        file_count: 0,
    };

    store::insert_vault(config, &vault).map_err(|e| e.to_string())?;
    Ok(RpcOutcome::single_log(
        vault,
        format!("vault created: {id}"),
    ))
}

pub async fn vault_list(config: &Config) -> Result<RpcOutcome<Vec<Vault>>, String> {
    let vaults = store::list_vaults(config).map_err(|e| e.to_string())?;
    log::debug!("[vault] list: count={}", vaults.len());
    Ok(RpcOutcome::single_log(vaults, "vaults listed"))
}

pub async fn vault_get(config: &Config, id: &str) -> Result<RpcOutcome<Vault>, String> {
    let id = id.trim();
    if id.is_empty() {
        return Err("vault_id must not be empty".to_string());
    }
    let vault = store::get_vault(config, id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("vault not found: {id}"))?;
    log::debug!("[vault] get: id={id} files={}", vault.file_count);
    Ok(RpcOutcome::single_log(vault, "vault loaded"))
}

pub async fn vault_files(config: &Config, id: &str) -> Result<RpcOutcome<Vec<VaultFile>>, String> {
    let id = id.trim();
    if id.is_empty() {
        return Err("vault_id must not be empty".to_string());
    }
    let files = store::list_files(config, id).map_err(|e| e.to_string())?;
    log::debug!("[vault] files: id={id} count={}", files.len());
    Ok(RpcOutcome::single_log(files, "vault files listed"))
}

pub async fn vault_remove(
    config: &Config,
    id: &str,
    purge_memory: bool,
) -> Result<RpcOutcome<serde_json::Value>, String> {
    let id = id.trim();
    if id.is_empty() {
        return Err("vault_id must not be empty".to_string());
    }
    let vault = store::get_vault(config, id).map_err(|e| e.to_string())?;
    let removed = store::remove_vault(config, id).map_err(|e| e.to_string())?;
    log::debug!("[vault] remove: id={id} removed={removed} purge_memory={purge_memory}");

    let mut purged = false;
    if removed && purge_memory {
        if let Some(v) = vault {
            if let Err(err) = clear_namespace(ClearNamespaceParams {
                namespace: v.namespace.clone(),
            })
            .await
            {
                log::warn!("[vault] remove: id={id} purge_namespace_failed err={err}");
                return Ok(RpcOutcome::single_log(
                    serde_json::json!({
                        "vault_id": id,
                        "removed": removed,
                        "purged": false,
                        "purge_error": err,
                    }),
                    format!("vault removed with purge error: {id}"),
                ));
            }
            purged = true;
        }
    }

    Ok(RpcOutcome::single_log(
        serde_json::json!({
            "vault_id": id,
            "removed": removed,
            "purged": purged,
        }),
        format!("vault removed: {id}"),
    ))
}

/// Enqueue an async vault sync. Returns immediately with a job
/// handle the caller polls via [`vault_sync_status`].
///
/// Previously this RPC blocked until the entire directory walk
/// completed — for a vault with ~16 supported files (HTML/MD) at
/// roughly 1–2 minutes per file (chunking + embedding + tree
/// extract jobs through a singleton ingest lock), that was a
/// 20–30 minute synchronous RPC, with no per-file progress visible
/// to the UI. The user reported "ingestion of the vault I added
/// seems to have stopped based on my laptop being silent again"
/// because the worker was draining the LLM extract queue between
/// embed bursts and there was no surface for that. Decoupling
/// surfaces the live state: the RPC enqueues, the registry's
/// process-wide worker drains, and the UI polls
/// [`vault_sync_status`] for progress + final report.
///
/// Coalesce: if a job is already pending/running for the same
/// vault, the existing handle is returned (no duplicate worker
/// spawn). Spam-clicking Sync is safe.
pub async fn vault_sync(
    config: &Config,
    id: &str,
) -> Result<RpcOutcome<VaultSyncJobHandle>, String> {
    let id = id.trim();
    if id.is_empty() {
        return Err("vault_id must not be empty".to_string());
    }
    log::debug!("[vault] sync: enqueue request id={id}");
    let handle = jobs::enqueue(config, id).await?;
    let msg = format!(
        "vault sync enqueued — job_id={} vault_id={} status={}",
        handle.job_id,
        handle.vault_id,
        handle.status.as_str()
    );
    Ok(RpcOutcome::single_log(handle, msg))
}

/// Read the current state of a vault sync job. Returns `None` when
/// the job id is unknown (typo, or process restart cleared the
/// in-memory registry).
///
/// While the job is `Running`, `processed` / `current_file` /
/// `errors` advance with each file. Once `Completed` or `Failed`,
/// `report` carries the final `VaultSyncReport` and the job's
/// vault slot is released so a subsequent enqueue starts a fresh
/// job.
pub async fn vault_sync_status(
    _config: &Config,
    job_id: &str,
) -> Result<RpcOutcome<Option<VaultSyncJobSnapshot>>, String> {
    let job_id = job_id.trim();
    if job_id.is_empty() {
        return Err("job_id must not be empty".to_string());
    }
    let snap = jobs::snapshot(job_id);
    let msg = match snap.as_ref() {
        Some(s) => format!(
            "vault sync status job_id={} status={} processed={}",
            s.job_id,
            s.status.as_str(),
            s.processed
        ),
        None => format!("vault sync status unknown job_id={job_id}"),
    };
    Ok(RpcOutcome::single_log(snap, msg))
}

/// Enqueue a sync job for every registered vault. Per-vault
/// coalesce still applies, so calling this twice in a row returns
/// the same handles for any already-active jobs.
pub async fn vault_sync_all(
    config: &Config,
) -> Result<RpcOutcome<Vec<VaultSyncJobHandle>>, String> {
    log::debug!("[vault] sync_all: enqueue request");
    let handles = jobs::enqueue_all(config).await?;
    let msg = format!("vault sync_all enqueued {} job(s)", handles.len());
    Ok(RpcOutcome::single_log(handles, msg))
}
