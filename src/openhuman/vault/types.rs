use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A user-registered local folder whose files are mirrored into memory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Vault {
    pub id: String,
    pub name: String,
    pub root_path: String,
    pub namespace: String,
    pub include_globs: Vec<String>,
    pub exclude_globs: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub file_count: u64,
}

/// Per-file ledger entry used for dedup on re-sync.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VaultFile {
    pub vault_id: String,
    pub rel_path: String,
    pub document_id: String,
    pub content_hash: String,
    pub mtime_ms: i64,
    pub bytes: u64,
    pub ingested_at: DateTime<Utc>,
    pub status: VaultFileStatus,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VaultFileStatus {
    Ok,
    Skipped,
    Failed,
}

impl VaultFileStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }

    pub(crate) fn parse(raw: &str) -> Self {
        match raw {
            "skipped" => Self::Skipped,
            "failed" => Self::Failed,
            _ => Self::Ok,
        }
    }
}

/// Summary returned from `vault.sync`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct VaultSyncReport {
    pub vault_id: String,
    pub scanned: u64,
    pub ingested: u64,
    pub unchanged: u64,
    pub removed: u64,
    pub failed: u64,
    pub skipped_unsupported: u64,
    pub duration_ms: i64,
    pub errors: Vec<String>,
}

/// Lifecycle state for an async vault sync job.
///
/// Transitions:
/// - `Queued` → `Running` when the worker picks the job off the queue.
/// - `Running` → `Completed` on a clean walk.
/// - `Running` → `Failed` if the walk panics or the vault no longer
///   exists when the worker tries to load it.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VaultSyncJobStatus {
    Queued,
    Running,
    Completed,
    Failed,
}

impl VaultSyncJobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    /// `true` for `Queued` or `Running` — i.e. a re-enqueue should
    /// coalesce onto this job instead of creating a new one.
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }
}

/// Snapshot of a vault sync job's live state. Returned by
/// `vault_sync_status` and emitted incrementally during the worker
/// drain so the UI can render a progress bar without holding the
/// full report.
///
/// When `status = Completed | Failed`, `report` carries the final
/// `VaultSyncReport` (or the partial state for failures).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VaultSyncJobSnapshot {
    pub job_id: String,
    pub vault_id: String,
    pub status: VaultSyncJobStatus,
    /// Files processed so far (ingested + unchanged + failed +
    /// skipped_unsupported). Equals `total` when the walk has
    /// completed and equals 0 when status is still `Queued`.
    pub processed: u64,
    /// Total supported files discovered by the directory walk. `None`
    /// before the walk has indexed the tree (status `Queued`, or
    /// `Running` in the first few hundred milliseconds before the
    /// first pre-pass). Some implementations skip the pre-pass and
    /// surface `processed` only — `total` may stay `None` for the
    /// full duration in that case.
    pub total: Option<u64>,
    /// Relative path of the file currently being ingested. `None`
    /// between files or after completion.
    pub current_file: Option<String>,
    pub errors: Vec<String>,
    /// Final report. Set when `status = Completed`; may also be set
    /// on `Failed` to carry the partial counts the worker had
    /// accumulated before the panic / vault-missing error.
    pub report: Option<VaultSyncReport>,
    /// Job lifetime markers.
    pub queued_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// Lightweight handle returned by `vault_sync` (the enqueue RPC).
/// Callers poll `vault_sync_status(job_id)` for the live state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VaultSyncJobHandle {
    pub job_id: String,
    pub vault_id: String,
    /// `Queued` for a fresh enqueue, `Running` when the call
    /// coalesced onto an in-flight job for the same vault.
    pub status: VaultSyncJobStatus,
}
