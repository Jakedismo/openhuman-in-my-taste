// Settings → Triggers panel for the direct-mode Composio webhook
// receiver. Owns the user-facing controls for the ngrok tunnel
// (authtoken, static domain) + receiver toggle, surfaces the live
// tunnel state, and runs a public-round-trip "Test tunnel" probe so
// the user can verify the wiring before Composio gets involved.
//
// The ngrok authtoken and Composio webhook signing secret are NEVER
// exposed back through any RPC — only a `has_authtoken` boolean is
// surfaced for the "Token stored ✓" indicator.
import { useCallback, useEffect, useRef, useState } from 'react';

import { useT } from '../../../lib/i18n/I18nContext';
import {
  type ComposioLocalWebhookStatus,
  openhumanComposioClearNgrokAuthtoken,
  openhumanComposioLocalWebhookStart,
  openhumanComposioLocalWebhookStatus,
  openhumanComposioLocalWebhookStop,
  openhumanComposioLocalWebhookTest,
  openhumanComposioSetNgrokAuthtoken,
  openhumanComposioSetWebhookConfig,
} from '../../../utils/tauriCommands';
import SettingsHeader from '../components/SettingsHeader';
import { useSettingsNavigation } from '../hooks/useSettingsNavigation';

// Two polling cadences. While the tunnel is in a transient state
// (`connecting` or `idle` after a save) we want the UI to surface the
// flip to `ready` quickly — the ngrok control-plane connect usually
// finishes within 1–3 seconds, so a 4-second poll can make the user
// think the Test button is permanently disabled (Jokke hit exactly
// that — tunnel went Ready 1s after the status RPC returned, but the
// next poll wasn't due for another 3s and the button stayed greyed).
const POLL_INTERVAL_FAST_MS = 800;
const POLL_INTERVAL_SLOW_MS = 4000;

type SaveStatus = 'idle' | 'saving' | 'saved' | 'error';
type TestStatus = 'idle' | 'testing' | 'ok' | 'failed';

const TriggersPanel = () => {
  const { t } = useT();
  const { navigateBack, breadcrumbs } = useSettingsNavigation();

  const [status, setStatus] = useState<ComposioLocalWebhookStatus | null>(null);
  const [loading, setLoading] = useState(true);

  // Form drafts — initialized from the loaded status, then persisted
  // back via Save. Authtoken is a write-only field (we never get the
  // current value back, so the input shows blank with placeholder).
  const [enabled, setEnabled] = useState(false);
  const [ngrokDomain, setNgrokDomain] = useState('');
  const [port, setPort] = useState<number>(8765);
  const [authtokenDraft, setAuthtokenDraft] = useState('');

  const [saveStatus, setSaveStatus] = useState<SaveStatus>('idle');
  const [saveMessage, setSaveMessage] = useState<string | null>(null);
  const [testStatus, setTestStatus] = useState<TestStatus>('idle');
  const [testMessage, setTestMessage] = useState<string | null>(null);

  const pollTimer = useRef<ReturnType<typeof setInterval> | null>(null);
  const saveStatusTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Seed the form drafts once from the first status payload. After
  // that, polling refreshes only the read-only display fields
  // (tunnel_state, public_url, subscription_id, error) — the user's
  // in-flight checkbox/domain/port edits are preserved verbatim.
  // Without this latch the 4-second poll would clobber an unchecked
  // toggle as long as the tunnel was still running on disk.
  const hasSeededForm = useRef(false);

  const refreshStatus = useCallback(async () => {
    try {
      const res = await openhumanComposioLocalWebhookStatus();
      // The Rust op wraps the status payload via `to_json`, which
      // produces `{result: <ComposioLocalWebhookStatus>, logs: [...]}`
      // when the RpcOutcome carries log lines (this one does). The
      // payload lives at `res.result` — NOT `res.result.status`.
      // The original TS type erroneously implied a `{ status: … }`
      // sub-object that the wire format does not produce, so the
      // status display silently stayed empty after Save.
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const raw = res as any;
      const next: ComposioLocalWebhookStatus | undefined =
        raw?.result?.status ?? raw?.result ?? raw?.status ?? raw;
      if (next && typeof next === 'object' && 'tunnel_state' in next) {
        setStatus(next);
        if (!hasSeededForm.current) {
          setNgrokDomain(next.ngrok_domain);
          setPort(next.local_port);
          setEnabled(next.tunnel_state !== 'idle');
          hasSeededForm.current = true;
        }
      } else {
        console.warn('[TriggersPanel] unexpected status shape', res);
      }
    } catch (err) {
      console.warn('[TriggersPanel] failed to load status:', err);
    }
  }, []);

  useEffect(() => {
    let isMounted = true;
    (async () => {
      await refreshStatus();
      if (isMounted) setLoading(false);
    })();
    return () => {
      isMounted = false;
      if (pollTimer.current !== null) clearInterval(pollTimer.current);
      if (saveStatusTimer.current !== null) clearTimeout(saveStatusTimer.current);
    };
  }, [refreshStatus]);

  // Switch the poll cadence based on what the tunnel is doing.
  // `connecting` / `idle` → fast (the user is waiting for the flip);
  // `ready` / `error` → slow (state is stable, no need to hammer the
  // status RPC).
  useEffect(() => {
    const interval =
      status?.tunnel_state === 'ready' || status?.tunnel_state === 'error'
        ? POLL_INTERVAL_SLOW_MS
        : POLL_INTERVAL_FAST_MS;
    if (pollTimer.current !== null) clearInterval(pollTimer.current);
    pollTimer.current = setInterval(refreshStatus, interval);
    return () => {
      if (pollTimer.current !== null) clearInterval(pollTimer.current);
    };
  }, [status?.tunnel_state, refreshStatus]);

  // Reset form drafts to the persisted values whenever a fresh status
  // arrives and the user hasn't started editing. Avoids the panel
  // looking "stale" right after a Save.
  useEffect(() => {
    if (status === null) return;
    setNgrokDomain(current => (current === '' ? status.ngrok_domain : current));
    setPort(current => (current === 8765 ? status.local_port : current));
  }, [status]);

  const startSaveStatusTimer = (next: SaveStatus, message: string | null) => {
    setSaveStatus(next);
    setSaveMessage(message);
    if (saveStatusTimer.current !== null) clearTimeout(saveStatusTimer.current);
    saveStatusTimer.current = setTimeout(() => {
      setSaveStatus('idle');
      setSaveMessage(null);
    }, 4000);
  };

  const handleSave = async () => {
    setSaveStatus('saving');
    setSaveMessage(null);
    try {
      if (authtokenDraft.trim() !== '') {
        await openhumanComposioSetNgrokAuthtoken(authtokenDraft.trim());
        setAuthtokenDraft('');
      }
      await openhumanComposioSetWebhookConfig({
        enabled,
        port,
        ngrok_domain: ngrokDomain.trim(),
      });
      // Persisted; if enabled, ensure the receiver picks up the
      // new domain / port without requiring an app restart.
      if (enabled) {
        await openhumanComposioLocalWebhookStop();
        await openhumanComposioLocalWebhookStart();
      } else {
        await openhumanComposioLocalWebhookStop();
      }
      await refreshStatus();
      startSaveStatusTimer('saved', null);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      console.warn('[TriggersPanel] failed to save:', err);
      startSaveStatusTimer('error', message);
    }
  };

  const handleTest = async () => {
    setTestStatus('testing');
    setTestMessage(null);
    try {
      const res = await openhumanComposioLocalWebhookTest();
      // The to_json wrapper produces `{result: T, logs: [...]}` when
      // an RpcOutcome carries log lines; `callCoreRpc` returns that
      // envelope as-is. So the actual payload may live at either
      // `res.result` (single-wrap) or `res.result.result` (when an
      // outer consumer further wrapped it). Probe both so a wire-shape
      // change doesn't silently fall through to "non-ok".
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const raw = res as any;
      const payload = raw?.result?.result ?? raw?.result ?? raw;
      console.debug('[TriggersPanel] test response', { res, payload });
      const ok = payload?.ok === true;
      const url = payload?.url ?? 'unknown URL';
      setTestStatus(ok ? 'ok' : 'failed');
      setTestMessage(
        ok
          ? `Round-trip OK at ${url}`
          : `Test returned non-ok response: ${JSON.stringify(payload).slice(0, 200)}`
      );
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      setTestStatus('failed');
      setTestMessage(message);
    }
  };

  const handleClearAuthtoken = async () => {
    try {
      await openhumanComposioClearNgrokAuthtoken();
      await refreshStatus();
      startSaveStatusTimer('saved', 'ngrok authtoken cleared');
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      startSaveStatusTimer('error', message);
    }
  };

  if (loading) {
    return (
      <div>
        <SettingsHeader
          title={t('composio.triggersTitle')}
          showBackButton
          onBack={navigateBack}
          breadcrumbs={breadcrumbs}
        />
        <div className="p-4">
          <p className="text-sm text-stone-500 dark:text-neutral-400">Loading…</p>
        </div>
      </div>
    );
  }

  const tunnelStateLabel = (() => {
    switch (status?.tunnel_state) {
      case 'ready':
        return 'Ready';
      case 'connecting':
        return 'Connecting…';
      case 'error':
        return 'Error';
      default:
        return 'Idle';
    }
  })();

  return (
    <div>
      <SettingsHeader
        title={t('composio.triggersTitle')}
        showBackButton
        onBack={navigateBack}
        breadcrumbs={breadcrumbs}
      />
      <div className="p-4 space-y-6 max-w-2xl">
        <section>
          <h3 className="text-base font-medium text-stone-900 dark:text-neutral-100">
            Direct-mode webhook receiver
          </h3>
          <p className="mt-1 text-sm text-stone-600 dark:text-neutral-400">
            Composio delivers trigger events to an HTTPS URL you control. OpenHuman embeds an
            ngrok tunnel so a free, persistent <code>.ngrok-free.dev</code> domain forwards to a
            local loopback listener that HMAC-verifies and dispatches events. No external
            webhook server, no Composio backend session needed.
          </p>
        </section>

        <section className="space-y-4 rounded-md border border-stone-200 dark:border-neutral-700 p-4">
          <h4 className="text-sm font-semibold text-stone-800 dark:text-neutral-200">
            Current status
          </h4>
          <dl className="grid grid-cols-[140px_1fr] gap-y-1 text-sm">
            <dt className="text-stone-500 dark:text-neutral-400">Tunnel</dt>
            <dd className="text-stone-900 dark:text-neutral-100">{tunnelStateLabel}</dd>
            <dt className="text-stone-500 dark:text-neutral-400">Public URL</dt>
            <dd className="text-stone-900 dark:text-neutral-100 break-all">
              {status?.public_url ?? '—'}
            </dd>
            <dt className="text-stone-500 dark:text-neutral-400">Subscription ID</dt>
            <dd className="text-stone-900 dark:text-neutral-100 break-all">
              {status?.subscription_id ? status.subscription_id : '—'}
            </dd>
            <dt className="text-stone-500 dark:text-neutral-400">Authtoken</dt>
            <dd className="text-stone-900 dark:text-neutral-100">
              {status?.has_authtoken ? 'Stored ✓' : 'Not set'}
            </dd>
            {status?.error && (
              <>
                <dt className="text-rose-700 dark:text-rose-300">Last error</dt>
                <dd className="text-rose-700 dark:text-rose-300 break-all">{status.error}</dd>
              </>
            )}
          </dl>
        </section>

        <section className="space-y-4 rounded-md border border-stone-200 dark:border-neutral-700 p-4">
          <h4 className="text-sm font-semibold text-stone-800 dark:text-neutral-200">
            Configuration
          </h4>

          <label className="flex items-center gap-2 text-sm">
            <input
              type="checkbox"
              checked={enabled}
              onChange={e => setEnabled(e.target.checked)}
            />
            <span>Enable local webhook receiver at app launch</span>
          </label>

          <div>
            <label className="block text-sm font-medium text-stone-700 dark:text-neutral-300">
              ngrok static domain
            </label>
            <input
              type="text"
              className="mt-1 w-full rounded border border-stone-300 dark:border-neutral-600 bg-white dark:bg-neutral-800 px-3 py-2 text-sm font-mono"
              placeholder="abc-123-xyz.ngrok-free.dev"
              value={ngrokDomain}
              onChange={e => setNgrokDomain(e.target.value)}
              spellCheck={false}
            />
            <p className="mt-1 text-xs text-stone-500 dark:text-neutral-400">
              Copy this from{' '}
              <a
                href="https://dashboard.ngrok.com/domains"
                className="underline"
                target="_blank"
                rel="noreferrer"
              >
                dashboard.ngrok.com/domains
              </a>
              . The free tier gives you one persistent domain.
            </p>
          </div>

          <div>
            <label className="block text-sm font-medium text-stone-700 dark:text-neutral-300">
              ngrok authtoken
            </label>
            <input
              type="password"
              className="mt-1 w-full rounded border border-stone-300 dark:border-neutral-600 bg-white dark:bg-neutral-800 px-3 py-2 text-sm font-mono"
              placeholder={status?.has_authtoken ? '••••••••• (stored)' : 'Paste your authtoken'}
              value={authtokenDraft}
              onChange={e => setAuthtokenDraft(e.target.value)}
              autoComplete="off"
              spellCheck={false}
            />
            <p className="mt-1 text-xs text-stone-500 dark:text-neutral-400">
              From{' '}
              <a
                href="https://dashboard.ngrok.com/get-started/your-authtoken"
                className="underline"
                target="_blank"
                rel="noreferrer"
              >
                dashboard.ngrok.com/get-started/your-authtoken
              </a>
              . Encrypted at rest; never returned through any RPC.
              {status?.has_authtoken && (
                <>
                  {' '}
                  <button
                    type="button"
                    className="text-rose-600 hover:underline"
                    onClick={handleClearAuthtoken}
                  >
                    Clear stored authtoken
                  </button>
                </>
              )}
            </p>
          </div>

          <details>
            <summary className="cursor-pointer text-xs text-stone-500 dark:text-neutral-400">
              Advanced
            </summary>
            <div className="mt-2">
              <label className="block text-sm font-medium text-stone-700 dark:text-neutral-300">
                Loopback port
              </label>
              <input
                type="number"
                min={1}
                max={65535}
                className="mt-1 w-32 rounded border border-stone-300 dark:border-neutral-600 bg-white dark:bg-neutral-800 px-3 py-2 text-sm font-mono"
                value={port}
                onChange={e => setPort(Number(e.target.value) || 8765)}
              />
              <p className="mt-1 text-xs text-stone-500 dark:text-neutral-400">
                Local port the listener binds to. Change only on a port collision.
              </p>
            </div>
          </details>

          <div className="flex items-center gap-3 pt-2">
            <button
              type="button"
              onClick={handleSave}
              disabled={saveStatus === 'saving'}
              className="rounded bg-primary-600 hover:bg-primary-700 text-white px-4 py-2 text-sm disabled:opacity-50"
            >
              {saveStatus === 'saving' ? 'Saving…' : 'Save'}
            </button>
            <button
              type="button"
              onClick={handleTest}
              disabled={testStatus === 'testing'}
              className="rounded border border-stone-300 dark:border-neutral-600 px-4 py-2 text-sm disabled:opacity-50"
              title="Send a healthz probe through ngrok back to loopback. Backend reports if the tunnel is not ready."
            >
              {testStatus === 'testing' ? 'Testing…' : 'Test tunnel'}
            </button>
            {saveStatus === 'saved' && (
              <span className="text-sm text-emerald-600 dark:text-emerald-400">
                {saveMessage ?? 'Saved.'}
              </span>
            )}
            {saveStatus === 'error' && (
              <span className="text-sm text-rose-600 dark:text-rose-400 break-all">
                {saveMessage ?? 'Save failed.'}
              </span>
            )}
            {testStatus === 'ok' && (
              <span className="text-sm text-emerald-600 dark:text-emerald-400 break-all">
                {testMessage}
              </span>
            )}
            {testStatus === 'failed' && (
              <span className="text-sm text-rose-600 dark:text-rose-400 break-all">
                {testMessage}
              </span>
            )}
          </div>
        </section>

        <section className="rounded-md border border-amber-200 dark:border-amber-700 bg-amber-50/60 dark:bg-amber-950/40 p-4 text-sm text-amber-900 dark:text-amber-200">
          <p>
            <strong>Quotas.</strong> The free ngrok tier allows 20 000 HTTP requests/month and
            1 GB bandwidth/month — comfortably above what Composio webhooks consume for personal
            use. Heavy multi-toolkit workloads may want a paid ngrok plan or a self-hosted
            tunnel; this is a v1 limit, not a permanent constraint.
          </p>
        </section>
      </div>
    </div>
  );
};

export default TriggersPanel;
