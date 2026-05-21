/**
 * Vault (knowledge vault) commands — folder-of-files ingested into memory.
 */
import { callCoreRpc } from '../../services/coreRpcClient';
import { CommandResponse, isTauri } from './common';

export interface CoreVault {
  id: string;
  name: string;
  root_path: string;
  namespace: string;
  include_globs: string[];
  exclude_globs: string[];
  created_at: string;
  last_synced_at?: string | null;
  file_count: number;
}

export type CoreVaultFileStatus = 'ok' | 'skipped' | 'failed';

export interface CoreVaultFile {
  vault_id: string;
  rel_path: string;
  document_id: string;
  content_hash: string;
  mtime_ms: number;
  bytes: number;
  ingested_at: string;
  status: CoreVaultFileStatus;
}

export interface CoreVaultSyncReport {
  vault_id: string;
  scanned: number;
  ingested: number;
  unchanged: number;
  removed: number;
  failed: number;
  skipped_unsupported: number;
  duration_ms: number;
  errors: string[];
}

/** Lifecycle of an async vault sync job. Matches the Rust enum. */
export type CoreVaultSyncJobStatus = 'queued' | 'running' | 'completed' | 'failed';

/**
 * Lightweight handle returned by `openhuman.vault_sync` and
 * `openhuman.vault_sync_all`. Callers poll
 * `openhuman.vault_sync_status(job_id)` for the live snapshot.
 */
export interface CoreVaultSyncJobHandle {
  job_id: string;
  vault_id: string;
  /**
   * `queued` for a fresh enqueue, `running` when the call coalesced
   * onto an in-flight job for the same vault.
   */
  status: CoreVaultSyncJobStatus;
}

/**
 * Live snapshot of a vault sync job — what the status RPC returns.
 * `report` is `null` while the job is queued/running and carries the
 * final `CoreVaultSyncReport` once status is `completed`/`failed`.
 */
export interface CoreVaultSyncJobSnapshot {
  job_id: string;
  vault_id: string;
  status: CoreVaultSyncJobStatus;
  processed: number;
  /**
   * Total file count from the directory walk's pre-pass, when
   * available. `null` while the walk is in progress (the current
   * worker doesn't run a pre-pass to avoid double-traversal).
   */
  total: number | null;
  /** Relative path of the file currently being ingested, if any. */
  current_file: string | null;
  errors: string[];
  report: CoreVaultSyncReport | null;
  queued_at: string;
  started_at: string | null;
  completed_at: string | null;
}

function ensureTauri() {
  if (!isTauri()) {
    throw new Error('Not running in Tauri');
  }
}

export async function openhumanVaultList(): Promise<CommandResponse<CoreVault[]>> {
  ensureTauri();
  return await callCoreRpc<CommandResponse<CoreVault[]>>({ method: 'openhuman.vault_list' });
}

export async function openhumanVaultCreate(params: {
  name: string;
  rootPath: string;
  includeGlobs?: string[];
  excludeGlobs?: string[];
}): Promise<CommandResponse<CoreVault>> {
  ensureTauri();
  return await callCoreRpc<CommandResponse<CoreVault>>({
    method: 'openhuman.vault_create',
    params: {
      name: params.name,
      root_path: params.rootPath,
      include_globs: params.includeGlobs ?? [],
      exclude_globs: params.excludeGlobs ?? [],
    },
  });
}

export async function openhumanVaultGet(vaultId: string): Promise<CommandResponse<CoreVault>> {
  ensureTauri();
  return await callCoreRpc<CommandResponse<CoreVault>>({
    method: 'openhuman.vault_get',
    params: { vault_id: vaultId },
  });
}

export async function openhumanVaultFiles(
  vaultId: string
): Promise<CommandResponse<CoreVaultFile[]>> {
  ensureTauri();
  return await callCoreRpc<CommandResponse<CoreVaultFile[]>>({
    method: 'openhuman.vault_files',
    params: { vault_id: vaultId },
  });
}

export async function openhumanVaultRemove(
  vaultId: string,
  purgeMemory: boolean
): Promise<CommandResponse<{ vault_id: string; removed: boolean; purged: boolean }>> {
  ensureTauri();
  return await callCoreRpc<
    CommandResponse<{ vault_id: string; removed: boolean; purged: boolean }>
  >({ method: 'openhuman.vault_remove', params: { vault_id: vaultId, purge_memory: purgeMemory } });
}

/**
 * Enqueue an async vault sync. Returns immediately with a job
 * handle the caller polls via `openhumanVaultSyncStatus`. Previously
 * this RPC blocked through the full directory walk (20-30 min for a
 * vault of moderate size); the new contract surfaces per-file
 * progress instead. Safe to spam-click — the worker coalesces
 * duplicate enqueues for the same vault.
 */
export async function openhumanVaultSync(
  vaultId: string
): Promise<CommandResponse<CoreVaultSyncJobHandle>> {
  ensureTauri();
  return await callCoreRpc<CommandResponse<CoreVaultSyncJobHandle>>({
    method: 'openhuman.vault_sync',
    params: { vault_id: vaultId },
  });
}

/**
 * Read the live snapshot for a vault sync job. UI polls this at
 * ~2s intervals while the status is `queued`/`running`, stops
 * polling on `completed`/`failed` and renders the final report.
 * Returns `null` (wrapped in the envelope) when the job id is
 * unknown — typo, or process restart cleared the in-memory
 * registry.
 */
export async function openhumanVaultSyncStatus(
  jobId: string
): Promise<CommandResponse<CoreVaultSyncJobSnapshot | null>> {
  ensureTauri();
  return await callCoreRpc<CommandResponse<CoreVaultSyncJobSnapshot | null>>({
    method: 'openhuman.vault_sync_status',
    params: { job_id: jobId },
  });
}

/**
 * Enqueue an async sync for every registered vault. Returns one
 * handle per vault; per-vault coalesce still applies, so calling
 * twice returns the same handles for any already-active jobs.
 */
export async function openhumanVaultSyncAll(): Promise<
  CommandResponse<CoreVaultSyncJobHandle[]>
> {
  ensureTauri();
  return await callCoreRpc<CommandResponse<CoreVaultSyncJobHandle[]>>({
    method: 'openhuman.vault_sync_all',
  });
}
