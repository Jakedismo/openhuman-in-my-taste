import debug from 'debug';
import { useCallback, useEffect, useRef, useState } from 'react';

import {
  type ComposioTriggerHistoryEntry,
  openhumanComposioListTriggerHistory,
} from '../utils/tauriCommands';

const log = debug('composio:history');
const POLL_MS = 5000;

export interface ComposeioTriggerHistoryState {
  archiveDir: string | null;
  currentDayFile: string | null;
  entries: ComposioTriggerHistoryEntry[];
  loading: boolean;
  error: string | null;
  coreConnected: boolean;
  refresh: () => Promise<void>;
}

export function useComposeioTriggerHistory(limit = 100): ComposeioTriggerHistoryState {
  const [archiveDir, setArchiveDir] = useState<string | null>(null);
  const [currentDayFile, setCurrentDayFile] = useState<string | null>(null);
  const [entries, setEntries] = useState<ComposioTriggerHistoryEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [coreConnected, setCoreConnected] = useState(false);
  const isRefreshingRef = useRef(false);

  // [direct-mode triggers] The archive store is populated by the
  // local webhook receiver dispatching to the event bus — it has
  // no dependency on a backend session. Gate-skipping the
  // sessionToken check so direct-mode users (BYO Composio key, no
  // OpenHuman backend sign-in) can see their trigger history
  // populate as events land.
  const refresh = useCallback(async () => {
    if (isRefreshingRef.current) {
      return;
    }
    isRefreshingRef.current = true;
    setLoading(true);
    try {
      const response = await openhumanComposioListTriggerHistory(limit);
      // Wire shape: `to_json` returns `{result: T, logs: [...]}` when
      // the RpcOutcome carries logs (this one does), so the payload
      // lives at `response.result`. The original code went one level
      // too deep — harmless when sessionToken was present (the page
      // never rendered) but breaks in direct mode where the page
      // does render.
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const raw = response as any;
      const result = raw?.result?.result ?? raw?.result ?? raw;
      if (!result || typeof result !== 'object' || !('archive_dir' in result)) {
        log('trigger history response had unexpected shape: %o', response);
        return;
      }
      setArchiveDir(result.archive_dir);
      setCurrentDayFile(result.current_day_file);
      setEntries(result.entries);
      setError(null);
      setCoreConnected(true);
      log('loaded %d composio trigger entries', result.entries.length);
    } catch (refreshError) {
      const message =
        refreshError instanceof Error ? refreshError.message : 'Failed to load Composio history';
      setError(message);
      setCoreConnected(false);
      log('failed to load trigger history: %s', message);
    } finally {
      isRefreshingRef.current = false;
      setLoading(false);
    }
  }, [limit]);

  useEffect(() => {
    void refresh();
    const timer = window.setInterval(() => {
      void refresh();
    }, POLL_MS);

    return () => {
      window.clearInterval(timer);
    };
  }, [refresh]);

  return { archiveDir, currentDayFile, entries, loading, error, coreConnected, refresh };
}
