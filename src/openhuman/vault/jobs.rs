//! Async vault-sync worker.
//!
//! Replaces the blocking `vault_sync` RPC contract with an
//! enqueue-and-poll pattern. The process-wide [`JobRegistry`]
//! singleton owns:
//!
//! - the per-job state snapshots (`job_id` → `Mutex<JobState>`),
//! - a coalesce map (`vault_id` → `active_job_id`) so spamming Sync
//!   on the same vault returns the same handle instead of fanning
//!   out N concurrent walks,
//! - a tokio task spawned per job that drives `sync::sync_vault`
//!   through a progress callback which updates the job snapshot
//!   after every file.
//!
//! The `IngestionState::acquire()` lock inside `doc_ingest` already
//! serialises the heavy embedding step process-wide, so running
//! several sync jobs in parallel doesn't actually parallelise the
//! heavy work — the registry deliberately doesn't try. Jobs spawn
//! their own task each but compete for the same singleton ingest
//! lock; in practice they drain effectively FIFO.
//!
//! No on-disk persistence: jobs live for the lifetime of the
//! process. An app restart mid-sync loses the in-flight job
//! snapshot, but the next enqueue resumes correctly because the
//! `vault_files` ledger dedup (hash + mtime) skips files that the
//! previous run already finished — only the current in-flight file
//! is redone.
//!
//! Failure mode: if the spawned task panics (genuinely abnormal —
//! `sync::sync_vault` already swallows per-file errors into the
//! report), the registry catches via `JoinHandle::is_finished` on a
//! best-effort drain pass and flags the job `Failed`. A panic that
//! aborts the runtime is on the operator to surface from the
//! process supervisor; this module doesn't try to be a panic-
//! recovery framework.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use chrono::Utc;
use parking_lot::Mutex;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::openhuman::config::Config;

use super::store;
use super::sync;
use super::types::{
    VaultSyncJobHandle, VaultSyncJobSnapshot, VaultSyncJobStatus, VaultSyncReport,
};

/// Per-job mutable state. Held behind an `Arc<Mutex<_>>` so the
/// worker task and the status RPC can both reach it cheaply.
#[derive(Debug, Clone)]
struct JobState {
    snapshot: VaultSyncJobSnapshot,
}

/// Process-wide registry. Lazily initialised by [`init`] (or by the
/// first enqueue when [`init`] hasn't been called yet — see
/// [`registry`]).
struct JobRegistry {
    /// `job_id` → state.
    jobs: Mutex<HashMap<String, Arc<Mutex<JobState>>>>,
    /// `vault_id` → currently-active `job_id`. Removed when the job
    /// reaches a terminal state so a subsequent sync request gets a
    /// fresh job.
    active_by_vault: Mutex<HashMap<String, String>>,
    /// `job_id` → spawned task handle. Used only for liveness
    /// inspection (`is_finished` to detect panicked tasks). The
    /// handle does not own the job state; on panic the snapshot is
    /// flipped to `Failed` from the worker's completion guard, not
    /// from polling this map.
    handles: Mutex<HashMap<String, JoinHandle<()>>>,
}

static REGISTRY: OnceLock<JobRegistry> = OnceLock::new();

fn registry() -> &'static JobRegistry {
    REGISTRY.get_or_init(|| JobRegistry {
        jobs: Mutex::new(HashMap::new()),
        active_by_vault: Mutex::new(HashMap::new()),
        handles: Mutex::new(HashMap::new()),
    })
}

/// Idempotent — safe to call multiple times. Reserved for parity
/// with `channels::runtime::listener_registry::init`. Today the
/// registry self-initialises on first use; this function is here
/// so a future startup wiring change (e.g. pre-warming the queue)
/// has a stable entry point.
pub fn init() {
    let _ = registry();
}

/// Enqueue a sync job for `vault_id`. If a job is already in flight
/// (`Queued` or `Running`) for the same vault, returns the existing
/// handle instead of spawning a duplicate worker — the UI's
/// "spam-click Sync" case lands here.
///
/// Returns `Err` only when the vault id is malformed or the worker
/// cannot find the vault row in storage. Runtime ingest errors
/// surface via the job's `errors` list, not here.
pub async fn enqueue(config: &Config, vault_id: &str) -> Result<VaultSyncJobHandle, String> {
    let vault_id = vault_id.trim().to_string();
    if vault_id.is_empty() {
        return Err("vault_id must not be empty".to_string());
    }

    // Coalesce: if a job is already in queued/running state for this
    // vault, return its handle.
    {
        let active = registry().active_by_vault.lock();
        if let Some(existing) = active.get(&vault_id) {
            if let Some(state) = registry().jobs.lock().get(existing).cloned() {
                let snap = state.lock().snapshot.clone();
                if snap.status.is_active() {
                    log::debug!(
                        "[vault:jobs] coalesce vault_id={} existing_job_id={} status={}",
                        vault_id,
                        snap.job_id,
                        snap.status.as_str()
                    );
                    return Ok(VaultSyncJobHandle {
                        job_id: snap.job_id,
                        vault_id: snap.vault_id,
                        status: snap.status,
                    });
                }
            }
        }
    }

    // Validate the vault exists before spawning. The worker re-loads
    // it (state may have changed between enqueue and execution) but
    // failing here keeps the obvious "typo'd id" case from spawning
    // a doomed task.
    let vault = store::get_vault(config, &vault_id)
        .map_err(|e| format!("vault store lookup failed: {e}"))?
        .ok_or_else(|| format!("vault not found: {vault_id}"))?;

    let job_id = format!("vsj_{}", Uuid::new_v4());
    let now = Utc::now();
    let initial = VaultSyncJobSnapshot {
        job_id: job_id.clone(),
        vault_id: vault_id.clone(),
        status: VaultSyncJobStatus::Queued,
        processed: 0,
        total: None,
        current_file: None,
        errors: Vec::new(),
        report: None,
        queued_at: now,
        started_at: None,
        completed_at: None,
    };

    let state = Arc::new(Mutex::new(JobState {
        snapshot: initial.clone(),
    }));

    {
        let mut jobs = registry().jobs.lock();
        jobs.insert(job_id.clone(), Arc::clone(&state));
    }
    {
        let mut active = registry().active_by_vault.lock();
        active.insert(vault_id.clone(), job_id.clone());
    }

    // Spawn the worker. It clones what it needs so the future is
    // 'static.
    let cfg_owned = config.clone();
    let vault_owned = vault.clone();
    let job_state = Arc::clone(&state);
    let job_id_for_task = job_id.clone();
    let vault_id_for_task = vault_id.clone();
    let handle = tokio::spawn(async move {
        run_job(cfg_owned, vault_owned, job_id_for_task, vault_id_for_task, job_state).await;
    });
    {
        let mut handles = registry().handles.lock();
        handles.insert(job_id.clone(), handle);
    }

    log::info!(
        "[vault:jobs] enqueued job_id={} vault_id={} (name={})",
        job_id,
        vault_id,
        vault.name
    );

    Ok(VaultSyncJobHandle {
        job_id,
        vault_id,
        status: VaultSyncJobStatus::Queued,
    })
}

/// Enqueue a sync job for every registered vault. Returns one
/// handle per vault. Idempotent per-vault via the same coalesce
/// logic [`enqueue`] uses.
pub async fn enqueue_all(config: &Config) -> Result<Vec<VaultSyncJobHandle>, String> {
    let vaults = store::list_vaults(config).map_err(|e| format!("list_vaults failed: {e}"))?;
    let mut handles = Vec::with_capacity(vaults.len());
    for v in vaults {
        match enqueue(config, &v.id).await {
            Ok(h) => handles.push(h),
            Err(e) => log::warn!(
                "[vault:jobs] enqueue failed during enqueue_all vault_id={} err={e}",
                v.id
            ),
        }
    }
    Ok(handles)
}

/// Read the current snapshot for `job_id`. Returns `None` when the
/// job id is unknown (typo, expired snapshot — today snapshots live
/// for the lifetime of the process so this only fires for genuine
/// typos).
pub fn snapshot(job_id: &str) -> Option<VaultSyncJobSnapshot> {
    registry()
        .jobs
        .lock()
        .get(job_id)
        .map(|state| state.lock().snapshot.clone())
}

/// Per-file progress callback handed to `sync::sync_vault`. Updates
/// the shared state under a short-lived lock and emits a debug log
/// for greppability.
fn make_progress_callback(state: Arc<Mutex<JobState>>) -> sync::ProgressCallback {
    Arc::new(move |update| {
        let mut guard = state.lock();
        guard.snapshot.processed = update.processed;
        guard.snapshot.current_file = update.current_file.clone();
        if let Some(total) = update.total {
            guard.snapshot.total = Some(total);
        }
        if let Some(err) = update.last_error.as_ref() {
            guard.snapshot.errors.push(err.clone());
        }
        log::debug!(
            "[vault:jobs] progress job_id={} processed={} total={:?} current_file={:?}",
            guard.snapshot.job_id,
            guard.snapshot.processed,
            guard.snapshot.total,
            guard.snapshot.current_file,
        );
    })
}

async fn run_job(
    config: Config,
    vault: super::types::Vault,
    job_id: String,
    vault_id: String,
    state: Arc<Mutex<JobState>>,
) {
    {
        let mut guard = state.lock();
        guard.snapshot.status = VaultSyncJobStatus::Running;
        guard.snapshot.started_at = Some(Utc::now());
    }
    log::info!(
        "[vault:jobs] starting job_id={} vault_id={} root={}",
        job_id,
        vault_id,
        vault.root_path
    );

    let progress = make_progress_callback(Arc::clone(&state));
    let report: VaultSyncReport = sync::sync_vault_with_progress(&config, &vault, progress).await;

    {
        let mut guard = state.lock();
        guard.snapshot.processed = report.scanned;
        if guard.snapshot.total.is_none() {
            guard.snapshot.total = Some(report.scanned);
        }
        guard.snapshot.current_file = None;
        guard.snapshot.status = VaultSyncJobStatus::Completed;
        guard.snapshot.completed_at = Some(Utc::now());
        guard.snapshot.report = Some(report);
    }
    {
        let mut active = registry().active_by_vault.lock();
        if let Some(current) = active.get(&vault_id) {
            if current == &job_id {
                active.remove(&vault_id);
            }
        }
    }
    log::info!(
        "[vault:jobs] completed job_id={} vault_id={}",
        job_id,
        vault_id
    );
}

#[cfg(test)]
pub(crate) fn reset_for_test() {
    let reg = registry();
    reg.jobs.lock().clear();
    reg.active_by_vault.lock().clear();
    reg.handles.lock().clear();
}
