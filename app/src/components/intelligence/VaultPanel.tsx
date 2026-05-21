/**
 * Knowledge vaults — point the assistant at a local folder and have its
 * files mirrored into memory under namespace `vault:<id>`. Sits inside
 * the Intelligence ▸ Memory tab.
 *
 * Sync is async: the panel enqueues a job via `openhumanVaultSync` and
 * polls `openhumanVaultSyncStatus(job_id)` every 2 s until the snapshot
 * reaches `completed` or `failed`. While running, the row shows a
 * progress bar with `processed / total` (or just `processed` when the
 * walk hasn't populated `total`) plus a tooltip with the current file
 * path. Spam-clicking Sync is safe — the backend coalesces duplicate
 * enqueues for the same vault and the panel just resumes polling.
 */
import { useCallback, useEffect, useRef, useState } from 'react';

import type { ToastNotification } from '../../types/intelligence';
import {
  type CoreVault,
  type CoreVaultSyncJobSnapshot,
  openhumanVaultCreate,
  openhumanVaultList,
  openhumanVaultRemove,
  openhumanVaultSync,
  openhumanVaultSyncAll,
  openhumanVaultSyncStatus,
} from '../../utils/tauriCommands/vault';

interface VaultPanelProps {
  onToast?: (toast: Omit<ToastNotification, 'id'>) => void;
}

/** How often we re-poll an in-flight sync job. */
const POLL_INTERVAL_MS = 2_000;

export function VaultPanel({ onToast }: VaultPanelProps) {
  const [vaults, setVaults] = useState<CoreVault[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [busy, setBusy] = useState<Record<string, 'remove' | undefined>>({});
  const [creating, setCreating] = useState(false);
  const [showForm, setShowForm] = useState(false);
  const [syncingAll, setSyncingAll] = useState(false);
  const [newName, setNewName] = useState('');
  const [newPath, setNewPath] = useState('');
  const [newExcludes, setNewExcludes] = useState('');
  /** Per-vault current job snapshot. `null` when no sync is in flight. */
  const [syncJobs, setSyncJobs] = useState<Record<string, CoreVaultSyncJobSnapshot | null>>(
    {}
  );
  /**
   * Active poll timers, keyed by `job_id`. Held in a ref because we
   * never want a re-render just because we set/clear a timeout —
   * `useState` would loop us through the effect cleanup.
   */
  const pollTimers = useRef<Record<string, number>>({});
  /** Mounted guard so async resolves don't try to setState after unmount. */
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      for (const id of Object.keys(pollTimers.current)) {
        window.clearTimeout(pollTimers.current[id]);
      }
      pollTimers.current = {};
    };
  }, []);

  const reload = useCallback(async () => {
    setLoading(true);
    setLoadError(null);
    try {
      const resp = await openhumanVaultList();
      if (!mounted.current) return;
      setVaults(resp.result);
    } catch (err) {
      console.error('[ui-flow][vault-panel] list failed', err);
      if (mounted.current) {
        setLoadError(err instanceof Error ? err.message : String(err));
      }
    } finally {
      if (mounted.current) setLoading(false);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  /**
   * Poll `vault_sync_status` once. If still queued/running, schedule
   * the next tick. On completion, surface a toast, clear the in-flight
   * state, and refresh the vault list (file_count + last_synced_at
   * land via the underlying touch in `sync_vault`).
   */
  const pollSync = useCallback(
    async (jobId: string, vaultId: string, vaultName: string) => {
      try {
        const resp = await openhumanVaultSyncStatus(jobId);
        if (!mounted.current) return;
        const snap = resp.result;
        if (!snap) {
          // Job id unknown — likely a process restart cleared the
          // registry. Drop the in-flight state silently.
          setSyncJobs(prev => ({ ...prev, [vaultId]: null }));
          delete pollTimers.current[jobId];
          return;
        }
        setSyncJobs(prev => ({ ...prev, [vaultId]: snap }));

        if (snap.status === 'queued' || snap.status === 'running') {
          pollTimers.current[jobId] = window.setTimeout(() => {
            void pollSync(jobId, vaultId, vaultName);
          }, POLL_INTERVAL_MS);
          return;
        }

        // Terminal: completed | failed. Surface a toast with the
        // final report counts if we have one.
        delete pollTimers.current[jobId];
        const r = snap.report;
        if (snap.status === 'completed' && r) {
          onToast?.({
            type: r.failed > 0 ? 'info' : 'success',
            title: `Synced "${vaultName}"`,
            message:
              `Ingested ${r.ingested}, unchanged ${r.unchanged}, removed ${r.removed}` +
              (r.failed > 0 ? `, failed ${r.failed}` : '') +
              (r.skipped_unsupported > 0 ? `, skipped ${r.skipped_unsupported}` : '') +
              ` · ${(r.duration_ms / 1000).toFixed(1)}s`,
          });
        } else if (snap.status === 'failed') {
          onToast?.({
            type: 'error',
            title: `Sync failed for "${vaultName}"`,
            message: snap.errors.slice(-1)[0] ?? 'Sync job failed without a recorded error.',
          });
        }
        // Clear the in-flight snapshot a beat after the toast so the
        // progress bar collapses on the next render.
        setSyncJobs(prev => ({ ...prev, [vaultId]: null }));
        await reload();
      } catch (err) {
        console.error('[ui-flow][vault-panel] sync status poll failed', err);
        if (!mounted.current) return;
        delete pollTimers.current[jobId];
        setSyncJobs(prev => ({ ...prev, [vaultId]: null }));
        onToast?.({
          type: 'error',
          title: 'Sync polling failed',
          message: err instanceof Error ? err.message : String(err),
        });
      }
    },
    [onToast, reload]
  );

  const handleCreate = useCallback(
    async (event: React.FormEvent) => {
      event.preventDefault();
      const name = newName.trim();
      const rootPath = newPath.trim();
      if (!name || !rootPath) return;
      const excludeGlobs = newExcludes
        .split(',')
        .map(s => s.trim())
        .filter(Boolean);
      setCreating(true);
      try {
        const resp = await openhumanVaultCreate({ name, rootPath, excludeGlobs });
        onToast?.({
          type: 'success',
          title: 'Vault added',
          message: `Created "${resp.result.name}". Click Sync to ingest.`,
        });
        setNewName('');
        setNewPath('');
        setNewExcludes('');
        setShowForm(false);
        await reload();
      } catch (err) {
        console.error('[ui-flow][vault-panel] create failed', err);
        onToast?.({
          type: 'error',
          title: 'Could not add vault',
          message: err instanceof Error ? err.message : String(err),
        });
      } finally {
        setCreating(false);
      }
    },
    [newName, newPath, newExcludes, onToast, reload]
  );

  const handleSync = useCallback(
    async (vault: CoreVault) => {
      try {
        const resp = await openhumanVaultSync(vault.id);
        const handle = resp.result;
        // Seed an immediate snapshot from the handle so the progress
        // affordance lights up without waiting for the first poll
        // round-trip. `processed`/`total` are 0/null at this point.
        setSyncJobs(prev => ({
          ...prev,
          [vault.id]: {
            job_id: handle.job_id,
            vault_id: handle.vault_id,
            status: handle.status,
            processed: 0,
            total: null,
            current_file: null,
            errors: [],
            report: null,
            queued_at: new Date().toISOString(),
            started_at: null,
            completed_at: null,
          },
        }));
        void pollSync(handle.job_id, vault.id, vault.name);
      } catch (err) {
        console.error('[ui-flow][vault-panel] sync enqueue failed', err);
        onToast?.({
          type: 'error',
          title: 'Sync failed to start',
          message: err instanceof Error ? err.message : String(err),
        });
      }
    },
    [onToast, pollSync]
  );

  const handleSyncAll = useCallback(async () => {
    setSyncingAll(true);
    try {
      const resp = await openhumanVaultSyncAll();
      // Seed per-vault snapshots from the handles, then start
      // polling each. Coalesce already deduped on the backend; the
      // returned handles cover every registered vault.
      const seeded: Record<string, CoreVaultSyncJobSnapshot> = {};
      const now = new Date().toISOString();
      for (const h of resp.result) {
        seeded[h.vault_id] = {
          job_id: h.job_id,
          vault_id: h.vault_id,
          status: h.status,
          processed: 0,
          total: null,
          current_file: null,
          errors: [],
          report: null,
          queued_at: now,
          started_at: null,
          completed_at: null,
        };
      }
      setSyncJobs(prev => ({ ...prev, ...seeded }));
      const byVaultId = new Map(vaults.map(v => [v.id, v.name]));
      for (const h of resp.result) {
        void pollSync(h.job_id, h.vault_id, byVaultId.get(h.vault_id) ?? h.vault_id);
      }
      onToast?.({
        type: 'success',
        title: 'Sync all',
        message: `Enqueued ${resp.result.length} sync job(s).`,
      });
    } catch (err) {
      console.error('[ui-flow][vault-panel] sync_all failed', err);
      onToast?.({
        type: 'error',
        title: 'Sync all failed',
        message: err instanceof Error ? err.message : String(err),
      });
    } finally {
      setSyncingAll(false);
    }
  }, [onToast, pollSync, vaults]);

  const handleRemove = useCallback(
    async (vault: CoreVault) => {
      const purge = window.confirm(
        `Remove vault "${vault.name}"?\n\nClick OK to also purge its memory (delete all ${vault.file_count} ingested document(s)).\nClick Cancel to keep the documents in memory.`
      );
      const ok = window.confirm(`Really remove vault "${vault.name}"?`);
      if (!ok) return;
      setBusy(b => ({ ...b, [vault.id]: 'remove' }));
      try {
        await openhumanVaultRemove(vault.id, purge);
        onToast?.({
          type: 'success',
          title: 'Vault removed',
          message: purge
            ? `Removed "${vault.name}" and purged its memory.`
            : `Removed "${vault.name}". Documents kept in memory.`,
        });
        await reload();
      } catch (err) {
        console.error('[ui-flow][vault-panel] remove failed', err);
        onToast?.({
          type: 'error',
          title: 'Could not remove vault',
          message: err instanceof Error ? err.message : String(err),
        });
      } finally {
        setBusy(b => ({ ...b, [vault.id]: undefined }));
      }
    },
    [onToast, reload]
  );

  return (
    <div
      className="rounded-lg border border-stone-200 dark:border-neutral-800 bg-white dark:bg-neutral-900 p-4 shadow-sm"
      data-testid="vault-panel">
      <div className="mb-3 flex items-center justify-between gap-3">
        <div>
          <h3 className="text-sm font-semibold text-stone-800 dark:text-neutral-100">
            Knowledge vaults
          </h3>
          <p className="text-xs text-stone-500 dark:text-neutral-400">
            Point at a local folder; files are chunked and mirrored into memory.
          </p>
        </div>
        <div className="flex items-center gap-2">
          {vaults.length > 1 ? (
            <button
              type="button"
              onClick={() => void handleSyncAll()}
              disabled={syncingAll}
              className="inline-flex items-center gap-1 rounded-md border border-primary-300 bg-white dark:bg-neutral-900
                         px-3 py-1.5 text-xs font-semibold text-primary-700 dark:text-primary-300 shadow-sm
                         transition-colors hover:bg-primary-50 dark:hover:bg-primary-500/15
                         focus:outline-none focus:ring-2 focus:ring-primary-200
                         disabled:cursor-not-allowed disabled:opacity-50"
              data-testid="vault-sync-all">
              {syncingAll ? 'Queuing…' : 'Sync all'}
            </button>
          ) : null}
          <button
            type="button"
            onClick={() => setShowForm(v => !v)}
            className="inline-flex items-center gap-1 rounded-md border border-primary-300 bg-white dark:bg-neutral-900
                       px-3 py-1.5 text-xs font-semibold text-primary-700 dark:text-primary-300 shadow-sm
                       transition-colors hover:bg-primary-50 dark:hover:bg-primary-500/15
                       focus:outline-none focus:ring-2 focus:ring-primary-200"
            data-testid="vault-add-toggle">
            {showForm ? 'Cancel' : '+ Add vault'}
          </button>
        </div>
      </div>

      {showForm ? (
        <form
          onSubmit={handleCreate}
          className="mb-3 space-y-2 rounded-md border border-stone-100 dark:border-neutral-800 bg-stone-50 dark:bg-neutral-800/60 p-3"
          data-testid="vault-add-form">
          <label className="block">
            <span className="text-xs font-medium text-stone-600 dark:text-neutral-300">Name</span>
            <input
              type="text"
              value={newName}
              onChange={e => setNewName(e.target.value)}
              required
              placeholder="My research notes"
              className="mt-1 w-full rounded border border-stone-200 dark:border-neutral-800 bg-white dark:bg-neutral-900 px-2 py-1.5 text-sm
                         focus:border-primary-400 focus:outline-none focus:ring-1 focus:ring-primary-200"
            />
          </label>
          <label className="block">
            <span className="text-xs font-medium text-stone-600 dark:text-neutral-300">
              Folder path (absolute)
            </span>
            <input
              type="text"
              value={newPath}
              onChange={e => setNewPath(e.target.value)}
              required
              placeholder="/Users/you/Documents/notes"
              className="mt-1 w-full rounded border border-stone-200 dark:border-neutral-800 bg-white dark:bg-neutral-900 px-2 py-1.5 font-mono text-xs
                         focus:border-primary-400 focus:outline-none focus:ring-1 focus:ring-primary-200"
            />
          </label>
          <label className="block">
            <span className="text-xs font-medium text-stone-600 dark:text-neutral-300">
              Excludes (comma-separated substrings, optional)
            </span>
            <input
              type="text"
              value={newExcludes}
              onChange={e => setNewExcludes(e.target.value)}
              placeholder="drafts/, .secret"
              className="mt-1 w-full rounded border border-stone-200 dark:border-neutral-800 bg-white dark:bg-neutral-900 px-2 py-1.5 text-xs
                         focus:border-primary-400 focus:outline-none focus:ring-1 focus:ring-primary-200"
            />
          </label>
          <div className="flex justify-end gap-2">
            <button
              type="submit"
              disabled={creating}
              className="rounded-md bg-primary-500 px-3 py-1.5 text-xs font-semibold text-white
                         shadow-sm transition-colors hover:bg-primary-600
                         disabled:cursor-not-allowed disabled:opacity-50">
              {creating ? 'Creating…' : 'Create vault'}
            </button>
          </div>
        </form>
      ) : null}

      {loading ? (
        <div className="py-4 text-center text-xs text-stone-400 dark:text-neutral-500">
          Loading vaults…
        </div>
      ) : loadError ? (
        <div className="rounded border border-coral-200 dark:border-coral-500/30 bg-coral-50 dark:bg-coral-500/10 px-3 py-2 text-xs text-coral-800">
          Failed to load vaults: {loadError}
        </div>
      ) : vaults.length === 0 ? (
        <div className="py-4 text-center text-xs text-stone-400 dark:text-neutral-500">
          No vaults yet. Add one above to start ingesting a folder.
        </div>
      ) : (
        <ul className="divide-y divide-stone-100 dark:divide-neutral-800" data-testid="vault-list">
          {vaults.map(v => {
            const state = busy[v.id];
            const snap = syncJobs[v.id] ?? null;
            const syncing = snap?.status === 'queued' || snap?.status === 'running';
            return (
              <li key={v.id} className="flex items-center justify-between gap-3 py-2">
                <div className="min-w-0 flex-1">
                  <div className="truncate text-sm font-medium text-stone-800 dark:text-neutral-100">
                    {v.name}
                  </div>
                  <div
                    className="truncate font-mono text-[11px] text-stone-500 dark:text-neutral-400"
                    title={v.root_path}>
                    {v.root_path}
                  </div>
                  <div className="mt-0.5 text-[11px] text-stone-400 dark:text-neutral-500">
                    {v.file_count.toLocaleString()} file(s) ·{' '}
                    {v.last_synced_at
                      ? `synced ${formatRelative(v.last_synced_at)}`
                      : 'never synced'}
                  </div>
                  {syncing && snap ? (
                    <div
                      className="mt-1 text-[11px] text-primary-700 dark:text-primary-300"
                      data-testid={`vault-sync-progress-${v.id}`}>
                      <span className="font-medium">
                        {snap.status === 'queued' ? 'Queued' : 'Syncing'} —{' '}
                        {snap.processed.toLocaleString()}
                        {snap.total != null ? ` / ${snap.total.toLocaleString()}` : ''} file(s)
                      </span>
                      {snap.current_file ? (
                        <span
                          className="ml-1 truncate text-stone-500 dark:text-neutral-400"
                          title={snap.current_file}>
                          · {snap.current_file}
                        </span>
                      ) : null}
                    </div>
                  ) : null}
                </div>
                <div className="flex items-center gap-2">
                  <button
                    type="button"
                    onClick={() => void handleSync(v)}
                    disabled={syncing || state === 'remove'}
                    className="rounded-md border border-primary-300 bg-white dark:bg-neutral-900 px-3 py-1.5 text-xs
                               font-semibold text-primary-700 dark:text-primary-300 shadow-sm transition-colors
                               hover:bg-primary-50 dark:hover:bg-primary-500/15 disabled:cursor-not-allowed disabled:opacity-50"
                    data-testid={`vault-sync-${v.id}`}>
                    {syncing ? 'Syncing…' : 'Sync'}
                  </button>
                  <button
                    type="button"
                    onClick={() => void handleRemove(v)}
                    disabled={syncing || state === 'remove'}
                    className="rounded-md border border-coral-200 dark:border-coral-500/30 bg-white dark:bg-neutral-900 px-3 py-1.5 text-xs
                               font-semibold text-coral-700 dark:text-coral-300 shadow-sm transition-colors
                               hover:bg-coral-50 dark:hover:bg-coral-500/10 disabled:cursor-not-allowed disabled:opacity-50">
                    {state === 'remove' ? 'Removing…' : 'Remove'}
                  </button>
                </div>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}

function formatRelative(iso: string): string {
  const t = new Date(iso).getTime();
  if (Number.isNaN(t)) return iso;
  const diff = Math.max(0, Date.now() - t);
  const sec = Math.floor(diff / 1000);
  if (sec < 60) return `${sec}s ago`;
  const min = Math.floor(sec / 60);
  if (min < 60) return `${min}m ago`;
  const hr = Math.floor(min / 60);
  if (hr < 24) return `${hr}h ago`;
  const day = Math.floor(hr / 24);
  return `${day}d ago`;
}
