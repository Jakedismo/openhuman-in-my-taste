import { useCallback, useEffect, useState } from 'react';

import {
  disableTrigger,
  enableTrigger,
  listAvailableTriggers,
  listTriggers,
} from '../../lib/composio/composioApi';
import { formatTriggerLabel } from '../../lib/composio/formatters';
import type { ComposioActiveTrigger, ComposioAvailableTrigger } from '../../lib/composio/types';
import { useT } from '../../lib/i18n/I18nContext';

/**
 * Stable signature for matching an `AvailableTrigger` to an
 * `ActiveTrigger`. Static toolkits key by slug; GitHub per-repo
 * triggers key by `slug::owner/repo` to disambiguate the same slug
 * across repos.
 */
export function triggerSignature(
  slug: string,
  scope: 'static' | 'github_repo',
  config?: { owner?: string; repo?: string }
): string {
  if (scope === 'github_repo' && config?.owner && config?.repo) {
    return `${slug.toUpperCase()}::${config.owner.toLowerCase()}/${config.repo.toLowerCase()}`;
  }
  return slug.toUpperCase();
}

export function activeTriggerSignature(t: ComposioActiveTrigger): string {
  const cfg = t.triggerConfig ?? {};
  const owner = typeof cfg.owner === 'string' ? cfg.owner : undefined;
  const repo = typeof cfg.repo === 'string' ? cfg.repo : undefined;
  if (owner && repo) {
    return `${t.slug.toUpperCase()}::${owner.toLowerCase()}/${repo.toLowerCase()}`;
  }
  return t.slug.toUpperCase();
}

export interface TriggerTogglesProps {
  toolkitSlug: string;
  toolkitName: string;
  connectionId: string;
}

export default function TriggerToggles({
  toolkitSlug,
  toolkitName,
  connectionId,
}: TriggerTogglesProps) {
  const { t } = useT();
  const [available, setAvailable] = useState<ComposioAvailableTrigger[] | null>(null);
  const [activeBySignature, setActiveBySignature] = useState<Map<string, ComposioActiveTrigger>>(
    new Map()
  );
  const [loadError, setLoadError] = useState<string | null>(null);
  const [pendingSignature, setPendingSignature] = useState<string | null>(null);
  const [rowError, setRowError] = useState<string | null>(null);
  // Inline configure form state for static triggers whose
  // `requiredConfigKeys` are non-empty (e.g. GitHub repo-scoped
  // triggers requiring `owner` + `repo`). `configFormFor` is the
  // signature of the trigger being configured, or null when no form
  // is open. Values are kept as a flat string map; we coerce to JSON
  // before posting since Composio v3's `trigger_config` is a free-
  // form JSON object — all the required keys we've seen so far for
  // the toolkits this UI surfaces are strings.
  const [configFormFor, setConfigFormFor] = useState<string | null>(null);
  const [configFormValues, setConfigFormValues] = useState<Record<string, string>>({});
  const [configFormSubmitting, setConfigFormSubmitting] = useState(false);

  // Load both lists in parallel on mount / when connection changes.
  useEffect(() => {
    let cancelled = false;
    setAvailable(null);
    setActiveBySignature(new Map());
    setLoadError(null);
    void (async () => {
      try {
        const [avail, active] = await Promise.all([
          listAvailableTriggers(toolkitSlug, connectionId),
          listTriggers(toolkitSlug),
        ]);
        if (cancelled) return;
        setAvailable(avail.triggers);
        const map = new Map<string, ComposioActiveTrigger>();
        for (const t of active.triggers) {
          if (t.connectionId && t.connectionId !== connectionId) continue;
          map.set(activeTriggerSignature(t), t);
        }
        setActiveBySignature(map);
      } catch (err) {
        if (cancelled) return;
        const msg = err instanceof Error ? err.message : String(err);
        setLoadError(`${t('composio.triggers.loadError')}: ${msg}`);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [toolkitSlug, connectionId]);

  const handleToggle = useCallback(
    async (entry: ComposioAvailableTrigger) => {
      const config = entry.scope === 'github_repo' ? entry.repo : undefined;
      const sig = triggerSignature(entry.slug, entry.scope, config);
      if (pendingSignature) return;
      setPendingSignature(sig);
      setRowError(null);

      const existing = activeBySignature.get(sig);
      try {
        if (existing) {
          await disableTrigger(existing.id);
          setActiveBySignature(prev => {
            const next = new Map(prev);
            next.delete(sig);
            return next;
          });
        } else {
          const triggerConfig =
            entry.scope === 'github_repo' && entry.repo
              ? { owner: entry.repo.owner, repo: entry.repo.repo }
              : entry.defaultConfig;
          const created = await enableTrigger(connectionId, entry.slug, triggerConfig);
          setActiveBySignature(prev => {
            const next = new Map(prev);
            next.set(sig, {
              id: created.triggerId,
              slug: created.slug,
              toolkit: toolkitSlug,
              connectionId: created.connectionId,
              ...(triggerConfig ? { triggerConfig } : {}),
            });
            return next;
          });
        }
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        const actionWord = existing ? t('common.disable') : t('common.enable');
        setRowError(`${actionWord} failed for ${formatTriggerLabel(entry.slug)}: ${msg}`);
      } finally {
        setPendingSignature(null);
      }
    },
    [activeBySignature, connectionId, pendingSignature, toolkitSlug]
  );

  /** Open the inline configure form for a static config-required
   *  trigger (e.g. GITHUB_COMMIT_EVENT needs owner + repo). Pre-fills
   *  any defaults from `defaultConfig` so users don't retype the
   *  common ones. */
  const openConfigForm = useCallback(
    (entry: ComposioAvailableTrigger) => {
      const sig = triggerSignature(entry.slug, entry.scope);
      const initial: Record<string, string> = {};
      for (const key of entry.requiredConfigKeys ?? []) {
        const existing = entry.defaultConfig?.[key];
        initial[key] = typeof existing === 'string' ? existing : '';
      }
      setConfigFormFor(sig);
      setConfigFormValues(initial);
      setRowError(null);
    },
    []
  );

  const closeConfigForm = useCallback(() => {
    setConfigFormFor(null);
    setConfigFormValues({});
    setConfigFormSubmitting(false);
  }, []);

  /** Submit the inline form: build trigger_config from the typed
   *  values + any non-required defaults, call enableTrigger, and add
   *  the resulting active trigger to the local map keyed by signature
   *  so the toggle flips on without a reload. */
  const submitConfigForm = useCallback(
    async (entry: ComposioAvailableTrigger) => {
      const sig = triggerSignature(entry.slug, entry.scope);
      const missing = (entry.requiredConfigKeys ?? []).filter(
        k => (configFormValues[k] ?? '').trim() === ''
      );
      if (missing.length > 0) {
        setRowError(`Fill in: ${missing.join(', ')}`);
        return;
      }
      setConfigFormSubmitting(true);
      setRowError(null);
      try {
        // Build the full trigger_config: defaults first (so non-required
        // keys come along), then the user's typed values overwrite the
        // required ones. Trim everything — Composio rejects leading/
        // trailing whitespace on the GitHub `owner` / `repo` fields.
        const merged: Record<string, unknown> = { ...(entry.defaultConfig ?? {}) };
        for (const key of entry.requiredConfigKeys ?? []) {
          merged[key] = (configFormValues[key] ?? '').trim();
        }
        const created = await enableTrigger(connectionId, entry.slug, merged);
        setActiveBySignature(prev => {
          const next = new Map(prev);
          next.set(sig, {
            id: created.triggerId,
            slug: created.slug,
            toolkit: toolkitSlug,
            connectionId: created.connectionId,
            triggerConfig: merged,
          });
          return next;
        });
        closeConfigForm();
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        setRowError(`${t('common.enable')} failed for ${formatTriggerLabel(entry.slug)}: ${msg}`);
      } finally {
        setConfigFormSubmitting(false);
      }
    },
    [closeConfigForm, configFormValues, connectionId, t, toolkitSlug]
  );

  if (loadError) {
    return (
      <div className="border-t border-stone-100 dark:border-neutral-800 pt-3 mt-1">
        <p className="text-[11px] text-coral-600">{loadError}</p>
      </div>
    );
  }

  if (available === null) {
    return (
      <div className="border-t border-stone-100 dark:border-neutral-800 pt-3 mt-1">
        <h3 className="text-xs font-semibold text-stone-700 dark:text-neutral-200 uppercase tracking-wide">
          {t('composio.triggers.heading')}
        </h3>
        <p className="mt-1 text-[11px] text-stone-400 dark:text-neutral-500">
          {t('composio.triggers.loading')}
        </p>
      </div>
    );
  }

  if (available.length === 0) {
    return (
      <div className="border-t border-stone-100 dark:border-neutral-800 pt-3 mt-1">
        <h3 className="text-xs font-semibold text-stone-700 dark:text-neutral-200 uppercase tracking-wide">
          {t('composio.triggers.heading')}
        </h3>
        <p className="mt-1 text-[11px] text-stone-400 dark:text-neutral-500">
          {`${t('composio.triggers.noneAvailable')} ${toolkitName}.`}
        </p>
      </div>
    );
  }

  return (
    <div
      className="border-t border-stone-100 dark:border-neutral-800 pt-3 mt-1 space-y-2"
      data-testid="trigger-toggles">
      <div className="flex items-baseline justify-between">
        <h3 className="text-xs font-semibold text-stone-700 dark:text-neutral-200 uppercase tracking-wide">
          {t('composio.triggers.heading')}
        </h3>
        <p className="text-[10px] text-stone-400 dark:text-neutral-500">{`${t('composio.triggers.listenFrom')} ${toolkitName}`}</p>
      </div>
      <ul className="space-y-1.5 max-h-56 overflow-y-auto pr-1">
        {available.map(entry => {
          const config = entry.scope === 'github_repo' ? entry.repo : undefined;
          const sig = triggerSignature(entry.slug, entry.scope, config);
          const enabled = activeBySignature.has(sig);
          const requiresConfig =
            (entry.requiredConfigKeys?.length ?? 0) > 0 && entry.scope === 'static';
          const isPending = pendingSignature === sig;
          const formOpen = configFormFor === sig;
          // Toggle is only hard-disabled while another row is pending.
          // Static config-required rows route their click through the
          // form opener instead of disabling — see onClick below.
          const disabled = pendingSignature !== null;

          const label =
            entry.scope === 'github_repo' && entry.repo
              ? `${entry.repo.owner}/${entry.repo.repo}`
              : formatTriggerLabel(entry.slug);
          const sub =
            entry.scope === 'github_repo'
              ? formatTriggerLabel(entry.slug)
              : requiresConfig && !enabled
                ? `Requires: ${(entry.requiredConfigKeys ?? []).join(', ')}`
                : '';
          const action = enabled ? t('common.disable') : t('common.enable');
          const triggerName = formatTriggerLabel(entry.slug);
          const ariaLabel =
            entry.scope === 'github_repo' && entry.repo
              ? `${action} ${triggerName} for ${entry.repo.owner}/${entry.repo.repo}`
              : `${action} ${triggerName}`;

          // Click routing:
          // - Enabled toggle on any row → disableTrigger (no form
          //   needed; we already have the trigger_config persisted).
          // - Disabled toggle on static config-required row → open
          //   the inline form to collect the required keys.
          // - Disabled toggle on any other row → enableTrigger
          //   directly (no config to collect).
          const onToggleClick = () => {
            if (enabled) {
              void handleToggle(entry);
              return;
            }
            if (requiresConfig) {
              if (formOpen) {
                closeConfigForm();
              } else {
                openConfigForm(entry);
              }
              return;
            }
            void handleToggle(entry);
          };

          return (
            <li
              key={sig}
              data-testid={`trigger-row-${sig}`}
              className="rounded-lg px-2 py-1.5 hover:bg-stone-50 dark:hover:bg-neutral-800/60">
              <div className="flex items-start justify-between gap-3">
                <div className="min-w-0 flex-1">
                  <span className="text-sm font-medium text-stone-900 dark:text-neutral-100 break-all">
                    {label}
                  </span>
                  {sub && (
                    <p className="text-[11px] text-stone-400 dark:text-neutral-500 leading-snug">
                      {sub}
                    </p>
                  )}
                </div>
                <button
                  type="button"
                  role="switch"
                  aria-checked={enabled}
                  aria-label={ariaLabel}
                  disabled={disabled}
                  onClick={onToggleClick}
                  className={`relative inline-flex h-5 w-9 shrink-0 cursor-pointer items-center rounded-full transition-colors focus:outline-none focus:ring-2 focus:ring-primary-500 focus:ring-offset-1 disabled:cursor-not-allowed disabled:opacity-50 ${
                    enabled ? 'bg-primary-500' : 'bg-stone-300 dark:bg-neutral-700'
                  }`}>
                  <span
                    className={`inline-block h-3.5 w-3.5 transform rounded-full bg-white dark:bg-neutral-900 shadow transition-transform ${
                      enabled ? 'translate-x-5' : 'translate-x-0.5'
                    } ${isPending ? 'animate-pulse' : ''}`}
                  />
                </button>
              </div>
              {formOpen && requiresConfig && (
                <form
                  data-testid={`trigger-config-form-${sig}`}
                  className="mt-2 rounded-md border border-stone-200 dark:border-neutral-700 bg-stone-50/60 dark:bg-neutral-900/40 p-2 space-y-2"
                  onSubmit={e => {
                    e.preventDefault();
                    void submitConfigForm(entry);
                  }}>
                  {(entry.requiredConfigKeys ?? []).map(key => (
                    <div key={key} className="flex items-center gap-2">
                      <label
                        htmlFor={`trig-cfg-${sig}-${key}`}
                        className="w-20 text-[11px] font-medium text-stone-600 dark:text-neutral-300">
                        {key}
                      </label>
                      <input
                        id={`trig-cfg-${sig}-${key}`}
                        type="text"
                        autoComplete="off"
                        spellCheck={false}
                        value={configFormValues[key] ?? ''}
                        onChange={ev =>
                          setConfigFormValues(prev => ({
                            ...prev,
                            [key]: ev.target.value,
                          }))
                        }
                        className="flex-1 rounded border border-stone-300 dark:border-neutral-600 bg-white dark:bg-neutral-800 px-2 py-1 text-xs font-mono"
                        placeholder={key === 'owner' ? 'jruokola' : key === 'repo' ? 'closedhuman' : ''}
                      />
                    </div>
                  ))}
                  <div className="flex items-center justify-end gap-2 pt-1">
                    <button
                      type="button"
                      onClick={closeConfigForm}
                      disabled={configFormSubmitting}
                      className="rounded border border-stone-300 dark:border-neutral-600 px-2 py-1 text-[11px] disabled:opacity-50">
                      Cancel
                    </button>
                    <button
                      type="submit"
                      disabled={configFormSubmitting}
                      className="rounded bg-primary-600 hover:bg-primary-700 px-3 py-1 text-[11px] text-white disabled:opacity-50">
                      {configFormSubmitting ? 'Enabling…' : 'Enable'}
                    </button>
                  </div>
                </form>
              )}
            </li>
          );
        })}
      </ul>
      {rowError && <p className="text-[11px] text-coral-600">{rowError}</p>}
    </div>
  );
}
