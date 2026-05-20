/**
 * Vitest for `<VaultPanel />`. Covers: load/empty/error states, the
 * add-vault form happy + error paths, the new async per-row sync
 * flow (enqueue → status poll → final toast), the "Sync all" batch
 * button, sync-enqueue + sync-poll failure surfaces, and the two
 * remove paths (purge=true / purge=false).
 *
 * Sync flow has been async since the queue+poll refactor: clicking
 * Sync enqueues a job and then `setTimeout(POLL_INTERVAL_MS)` -based
 * polling reads `openhumanVaultSyncStatus(job_id)` until the
 * snapshot reaches `completed` / `failed`. Tests use fake timers
 * to advance the poll loop deterministically.
 */
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { VaultPanel } from './VaultPanel';

const mockList = vi.fn();
const mockCreate = vi.fn();
const mockSync = vi.fn();
const mockSyncStatus = vi.fn();
const mockSyncAll = vi.fn();
const mockRemove = vi.fn();

vi.mock('../../utils/tauriCommands/vault', () => ({
  openhumanVaultList: (...args: unknown[]) => mockList(...args),
  openhumanVaultCreate: (...args: unknown[]) => mockCreate(...args),
  openhumanVaultSync: (...args: unknown[]) => mockSync(...args),
  openhumanVaultSyncStatus: (...args: unknown[]) => mockSyncStatus(...args),
  openhumanVaultSyncAll: (...args: unknown[]) => mockSyncAll(...args),
  openhumanVaultRemove: (...args: unknown[]) => mockRemove(...args),
}));

function vault(overrides: Record<string, unknown> = {}) {
  return {
    id: 'v-1',
    name: 'Notes',
    root_path: '/Users/me/notes',
    namespace: 'vault:v-1',
    include_globs: [],
    exclude_globs: [],
    created_at: '2026-05-17T10:00:00Z',
    last_synced_at: null,
    file_count: 0,
    ...overrides,
  };
}

function syncReport(overrides: Record<string, unknown> = {}) {
  return {
    vault_id: 'v-1',
    scanned: 4,
    ingested: 3,
    unchanged: 1,
    removed: 0,
    failed: 0,
    skipped_unsupported: 0,
    duration_ms: 1200,
    errors: [],
    ...overrides,
  };
}

function syncSnapshot(overrides: Record<string, unknown> = {}) {
  return {
    job_id: 'vsj_abc',
    vault_id: 'v-1',
    status: 'running' as const,
    processed: 1,
    total: null,
    current_file: 'notes.md',
    errors: [],
    report: null,
    queued_at: '2026-05-20T12:00:00Z',
    started_at: '2026-05-20T12:00:01Z',
    completed_at: null,
    ...overrides,
  };
}

describe('<VaultPanel />', () => {
  beforeEach(() => {
    mockList.mockReset();
    mockCreate.mockReset();
    mockSync.mockReset();
    mockSyncStatus.mockReset();
    mockSyncAll.mockReset();
    mockRemove.mockReset();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it('shows loading then empty state when list returns no vaults', async () => {
    mockList.mockResolvedValueOnce({ result: [], logs: [] });
    render(<VaultPanel />);
    expect(screen.getByText(/Loading vaults/)).toBeTruthy();
    await waitFor(() => screen.getByText(/No vaults yet/));
    expect(mockList).toHaveBeenCalledTimes(1);
  });

  it('renders an error banner when list fails', async () => {
    mockList.mockRejectedValueOnce(new Error('rpc down'));
    render(<VaultPanel />);
    await waitFor(() => screen.getByText(/Failed to load vaults/));
    expect(screen.getByText(/rpc down/)).toBeTruthy();
  });

  it('lists vaults with file count + relative last-synced label', async () => {
    vi.spyOn(Date, 'now').mockReturnValue(new Date('2026-05-17T10:05:00Z').getTime());
    mockList.mockResolvedValueOnce({
      result: [
        vault({ id: 'v-A', name: 'A', file_count: 42, last_synced_at: '2026-05-17T10:04:30Z' }),
      ],
      logs: [],
    });
    render(<VaultPanel />);
    await waitFor(() => screen.getByTestId('vault-list'));
    expect(screen.getByText('A')).toBeTruthy();
    expect(screen.getByText(/42 file/)).toBeTruthy();
    expect(screen.getByText(/synced 30s ago/)).toBeTruthy();
  });

  it('toggles the add form and creates a vault on submit', async () => {
    mockList
      .mockResolvedValueOnce({ result: [], logs: [] })
      .mockResolvedValueOnce({ result: [vault()], logs: [] });
    mockCreate.mockResolvedValueOnce({ result: vault(), logs: [] });
    const onToast = vi.fn();
    render(<VaultPanel onToast={onToast} />);
    await waitFor(() => screen.getByText(/No vaults yet/));

    fireEvent.click(screen.getByTestId('vault-add-toggle'));
    const form = screen.getByTestId('vault-add-form');
    const inputs = form.querySelectorAll('input');
    fireEvent.change(inputs[0], { target: { value: 'My notes' } });
    fireEvent.change(inputs[1], { target: { value: '/Users/me/notes' } });
    fireEvent.change(inputs[2], { target: { value: 'drafts, .secret' } });
    fireEvent.submit(form);

    await waitFor(() =>
      expect(mockCreate).toHaveBeenCalledWith({
        name: 'My notes',
        rootPath: '/Users/me/notes',
        excludeGlobs: ['drafts', '.secret'],
      })
    );
    expect(onToast).toHaveBeenCalledWith(
      expect.objectContaining({ type: 'success', title: 'Vault added' })
    );
    expect(mockList).toHaveBeenCalledTimes(2);
  });

  it('emits an error toast when create throws', async () => {
    mockList.mockResolvedValueOnce({ result: [], logs: [] });
    mockCreate.mockRejectedValueOnce(new Error('disk full'));
    const onToast = vi.fn();
    render(<VaultPanel onToast={onToast} />);
    await waitFor(() => screen.getByText(/No vaults yet/));

    fireEvent.click(screen.getByTestId('vault-add-toggle'));
    const form = screen.getByTestId('vault-add-form');
    const inputs = form.querySelectorAll('input');
    fireEvent.change(inputs[0], { target: { value: 'n' } });
    fireEvent.change(inputs[1], { target: { value: '/x' } });
    fireEvent.submit(form);

    await waitFor(() =>
      expect(onToast).toHaveBeenCalledWith(
        expect.objectContaining({ type: 'error', title: 'Could not add vault' })
      )
    );
  });

  it('enqueues a sync, fires the first status poll, and reports counts via toast on completion', async () => {
    // Real timers — the first poll fires synchronously (no setTimeout
    // gate). Mocking it to return a terminal `completed` snapshot
    // avoids the need to wind through the 2s poll loop, which fights
    // with `waitFor`'s own polling under fake timers.
    mockList
      .mockResolvedValueOnce({ result: [vault()], logs: [] })
      .mockResolvedValueOnce({ result: [vault()], logs: [] });
    mockSync.mockResolvedValueOnce({
      result: { job_id: 'vsj_abc', vault_id: 'v-1', status: 'queued' },
      logs: [],
    });
    mockSyncStatus.mockResolvedValueOnce({
      result: syncSnapshot({
        status: 'completed',
        processed: 4,
        current_file: null,
        report: syncReport(),
        completed_at: '2026-05-20T12:00:05Z',
      }),
      logs: [],
    });

    const onToast = vi.fn();
    render(<VaultPanel onToast={onToast} />);
    await waitFor(() => screen.getByTestId('vault-list'));

    fireEvent.click(screen.getByText('Sync'));
    await waitFor(() => expect(mockSync).toHaveBeenCalledWith('v-1'));
    await waitFor(() => expect(mockSyncStatus).toHaveBeenCalledWith('vsj_abc'));
    await waitFor(() =>
      expect(onToast).toHaveBeenCalledWith(
        expect.objectContaining({
          type: 'success',
          title: expect.stringContaining('Synced'),
          message: expect.stringContaining('Ingested 3'),
        })
      )
    );
  });

  it('uses info toast when sync reports failed files', async () => {
    mockList
      .mockResolvedValueOnce({ result: [vault()], logs: [] })
      .mockResolvedValueOnce({ result: [vault()], logs: [] });
    mockSync.mockResolvedValueOnce({
      result: { job_id: 'vsj_abc', vault_id: 'v-1', status: 'queued' },
      logs: [],
    });
    mockSyncStatus.mockResolvedValueOnce({
      result: syncSnapshot({
        status: 'completed',
        processed: 2,
        report: syncReport({
          scanned: 2,
          ingested: 1,
          unchanged: 0,
          failed: 1,
          errors: ['x.md: read failed'],
          duration_ms: 50,
        }),
        completed_at: '2026-05-20T12:00:05Z',
      }),
      logs: [],
    });

    const onToast = vi.fn();
    render(<VaultPanel onToast={onToast} />);
    await waitFor(() => screen.getByTestId('vault-list'));

    fireEvent.click(screen.getByText('Sync'));
    await waitFor(() =>
      expect(onToast).toHaveBeenCalledWith(
        expect.objectContaining({ type: 'info', message: expect.stringContaining('failed 1') })
      )
    );
  });

  it('emits error toast when the sync-enqueue RPC fails', async () => {
    mockList.mockResolvedValueOnce({ result: [vault()], logs: [] });
    mockSync.mockRejectedValueOnce(new Error('boom'));
    const onToast = vi.fn();
    render(<VaultPanel onToast={onToast} />);
    await waitFor(() => screen.getByTestId('vault-list'));

    fireEvent.click(screen.getByText('Sync'));
    await waitFor(() =>
      expect(onToast).toHaveBeenCalledWith(
        expect.objectContaining({ type: 'error', title: 'Sync failed to start' })
      )
    );
  });

  it('emits error toast when a status snapshot reports status=failed', async () => {
    mockList
      .mockResolvedValueOnce({ result: [vault()], logs: [] })
      .mockResolvedValueOnce({ result: [vault()], logs: [] });
    mockSync.mockResolvedValueOnce({
      result: { job_id: 'vsj_abc', vault_id: 'v-1', status: 'queued' },
      logs: [],
    });
    mockSyncStatus.mockResolvedValueOnce({
      result: syncSnapshot({
        status: 'failed',
        errors: ['root_path is not a directory'],
        completed_at: '2026-05-20T12:00:05Z',
      }),
      logs: [],
    });
    const onToast = vi.fn();
    render(<VaultPanel onToast={onToast} />);
    await waitFor(() => screen.getByTestId('vault-list'));

    fireEvent.click(screen.getByText('Sync'));
    await waitFor(() =>
      expect(onToast).toHaveBeenCalledWith(
        expect.objectContaining({
          type: 'error',
          title: expect.stringContaining('Sync failed for'),
          message: expect.stringContaining('not a directory'),
        })
      )
    );
  });

  it('"Sync all" button enqueues one job per vault and polls each', async () => {
    mockList
      .mockResolvedValueOnce({
        result: [
          vault({ id: 'v-1', name: 'A' }),
          vault({ id: 'v-2', name: 'B' }),
        ],
        logs: [],
      })
      .mockResolvedValueOnce({
        result: [
          vault({ id: 'v-1', name: 'A' }),
          vault({ id: 'v-2', name: 'B' }),
        ],
        logs: [],
      })
      .mockResolvedValueOnce({
        result: [
          vault({ id: 'v-1', name: 'A' }),
          vault({ id: 'v-2', name: 'B' }),
        ],
        logs: [],
      });
    mockSyncAll.mockResolvedValueOnce({
      result: [
        { job_id: 'vsj_a', vault_id: 'v-1', status: 'queued' },
        { job_id: 'vsj_b', vault_id: 'v-2', status: 'queued' },
      ],
      logs: [],
    });
    // Each status poll resolves immediately to a `completed` snapshot
    // so the polling loop terminates without waiting on setTimeout.
    mockSyncStatus.mockImplementation((jobId: string) =>
      Promise.resolve({
        result: syncSnapshot({
          job_id: jobId,
          vault_id: jobId === 'vsj_a' ? 'v-1' : 'v-2',
          status: 'completed',
          processed: 1,
          report: syncReport({ vault_id: jobId === 'vsj_a' ? 'v-1' : 'v-2' }),
        }),
        logs: [],
      })
    );
    const onToast = vi.fn();
    render(<VaultPanel onToast={onToast} />);
    await waitFor(() => screen.getByTestId('vault-list'));

    fireEvent.click(screen.getByTestId('vault-sync-all'));
    await waitFor(() => expect(mockSyncAll).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(mockSyncStatus.mock.calls.length).toBeGreaterThanOrEqual(2));
    expect(onToast).toHaveBeenCalledWith(
      expect.objectContaining({
        title: 'Sync all',
        message: expect.stringContaining('Enqueued 2'),
      })
    );
  });

  it('removes a vault with purge=true when both confirms accepted', async () => {
    mockList
      .mockResolvedValueOnce({ result: [vault()], logs: [] })
      .mockResolvedValueOnce({ result: [], logs: [] });
    mockRemove.mockResolvedValueOnce({
      result: { vault_id: 'v-1', removed: true, purged: true },
      logs: [],
    });
    const confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(true);
    const onToast = vi.fn();
    render(<VaultPanel onToast={onToast} />);
    await waitFor(() => screen.getByTestId('vault-list'));

    fireEvent.click(screen.getByText('Remove'));
    await waitFor(() => expect(mockRemove).toHaveBeenCalledWith('v-1', true));
    expect(onToast).toHaveBeenCalledWith(
      expect.objectContaining({ type: 'success', message: expect.stringContaining('purged') })
    );
    confirmSpy.mockRestore();
  });

  it('removes a vault with purge=false when first confirm denied', async () => {
    mockList
      .mockResolvedValueOnce({ result: [vault()], logs: [] })
      .mockResolvedValueOnce({ result: [], logs: [] });
    mockRemove.mockResolvedValueOnce({
      result: { vault_id: 'v-1', removed: true, purged: false },
      logs: [],
    });
    const confirmSpy = vi
      .spyOn(window, 'confirm')
      .mockReturnValueOnce(false)
      .mockReturnValueOnce(true);
    const onToast = vi.fn();
    render(<VaultPanel onToast={onToast} />);
    await waitFor(() => screen.getByTestId('vault-list'));

    fireEvent.click(screen.getByText('Remove'));
    await waitFor(() => expect(mockRemove).toHaveBeenCalledWith('v-1', false));
    expect(onToast).toHaveBeenCalledWith(
      expect.objectContaining({ message: expect.stringContaining('Documents kept') })
    );
    confirmSpy.mockRestore();
  });

  it('aborts remove when second confirm is denied', async () => {
    mockList.mockResolvedValueOnce({ result: [vault()], logs: [] });
    const confirmSpy = vi
      .spyOn(window, 'confirm')
      .mockReturnValueOnce(true)
      .mockReturnValueOnce(false);
    render(<VaultPanel />);
    await waitFor(() => screen.getByTestId('vault-list'));

    fireEvent.click(screen.getByText('Remove'));
    await Promise.resolve();
    expect(mockRemove).not.toHaveBeenCalled();
    confirmSpy.mockRestore();
  });

  it('emits error toast when remove RPC fails', async () => {
    mockList.mockResolvedValueOnce({ result: [vault()], logs: [] });
    mockRemove.mockRejectedValueOnce(new Error('locked'));
    const confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(true);
    const onToast = vi.fn();
    render(<VaultPanel onToast={onToast} />);
    await waitFor(() => screen.getByTestId('vault-list'));

    fireEvent.click(screen.getByText('Remove'));
    await waitFor(() =>
      expect(onToast).toHaveBeenCalledWith(
        expect.objectContaining({ type: 'error', title: 'Could not remove vault' })
      )
    );
    confirmSpy.mockRestore();
  });
});
