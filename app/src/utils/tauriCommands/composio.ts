import { callCoreRpc } from '../../services/coreRpcClient';
import { type CommandResponse, isTauri } from './common';

export interface ComposioTriggerHistoryEntry {
  received_at_ms: number;
  toolkit: string;
  trigger: string;
  metadata_id: string;
  metadata_uuid: string;
  payload: unknown;
}

export interface ComposioTriggerHistoryResult {
  archive_dir: string;
  current_day_file: string;
  entries: ComposioTriggerHistoryEntry[];
}

export async function openhumanComposioListTriggerHistory(
  limit = 100
): Promise<CommandResponse<{ result: ComposioTriggerHistoryResult }>> {
  if (!isTauri()) {
    throw new Error('Not running in Tauri');
  }

  return await callCoreRpc<CommandResponse<{ result: ComposioTriggerHistoryResult }>>({
    method: 'openhuman.composio_list_trigger_history',
    params: { limit },
  });
}

// ── Direct mode (BYO API key) ───────────────────────────────────────
//
// [composio-direct] These three RPCs back the Settings > Composio panel
// added in PR3 of #1710. The API key itself is *never* read back over
// RPC — only a boolean flag (`api_key_set`) so the UI can show a
// "Key stored ✓" / "No key set" status without exposing the secret.

export interface ComposioModeStatus {
  mode: 'backend' | 'direct' | string;
  api_key_set: boolean;
}

/// Read the current Composio routing mode and whether a direct-mode API
/// key is stored. The key itself is never returned.
export async function openhumanComposioGetMode(): Promise<CommandResponse<ComposioModeStatus>> {
  if (!isTauri()) {
    throw new Error('Not running in Tauri');
  }
  return await callCoreRpc<CommandResponse<ComposioModeStatus>>({
    method: 'openhuman.composio_get_mode',
  });
}

export interface ComposioSetApiKeyResult {
  stored: boolean;
  mode: string;
}

/// Persist a Composio API key for direct mode and (optionally) flip the
/// routing mode to "direct".
export async function openhumanComposioSetApiKey(
  apiKey: string,
  activateDirect = true
): Promise<CommandResponse<ComposioSetApiKeyResult>> {
  if (!isTauri()) {
    throw new Error('Not running in Tauri');
  }
  return await callCoreRpc<CommandResponse<ComposioSetApiKeyResult>>({
    method: 'openhuman.composio_set_api_key',
    params: { api_key: apiKey, activate_direct: activateDirect },
  });
}

/// Remove the stored direct-mode API key and reset the routing mode to
/// "backend".
export async function openhumanComposioClearApiKey(): Promise<
  CommandResponse<{ cleared: boolean; mode: string }>
> {
  if (!isTauri()) {
    throw new Error('Not running in Tauri');
  }
  return await callCoreRpc<CommandResponse<{ cleared: boolean; mode: string }>>({
    method: 'openhuman.composio_clear_api_key',
  });
}

// ── Local webhook receiver (direct-mode trigger delivery) ───────────
//
// These back the Settings → Triggers panel. The frontend never sees
// the ngrok authtoken or the Composio webhook signing secret — both
// are stored encrypted in AuthService and surfaced only via the
// `has_authtoken` boolean on status.

export type ComposioTunnelState = 'idle' | 'connecting' | 'ready' | 'error' | string;

export interface ComposioLocalWebhookStatus {
  tunnel_state: ComposioTunnelState;
  public_url: string | null;
  error: string | null;
  subscription_id: string;
  local_port: number;
  ngrok_domain: string;
  has_authtoken: boolean;
}

/// Snapshot of the local webhook receiver: tunnel state, configured
/// ngrok domain, persisted subscription ID, and whether an authtoken
/// has been stored. Token itself is never returned.
export async function openhumanComposioLocalWebhookStatus(): Promise<
  CommandResponse<{ status: ComposioLocalWebhookStatus }>
> {
  if (!isTauri()) {
    throw new Error('Not running in Tauri');
  }
  return await callCoreRpc<CommandResponse<{ status: ComposioLocalWebhookStatus }>>({
    method: 'openhuman.composio_local_webhook_status',
  });
}

/// Explicit (re)start of the receiver + tunnel. Idempotent.
export async function openhumanComposioLocalWebhookStart(): Promise<
  CommandResponse<{ status: ComposioLocalWebhookStatus }>
> {
  if (!isTauri()) {
    throw new Error('Not running in Tauri');
  }
  return await callCoreRpc<CommandResponse<{ status: ComposioLocalWebhookStatus }>>({
    method: 'openhuman.composio_local_webhook_start',
  });
}

/// Stop the tunnel + abort the local listener.
export async function openhumanComposioLocalWebhookStop(): Promise<
  CommandResponse<{ status: ComposioLocalWebhookStatus }>
> {
  if (!isTauri()) {
    throw new Error('Not running in Tauri');
  }
  return await callCoreRpc<CommandResponse<{ status: ComposioLocalWebhookStatus }>>({
    method: 'openhuman.composio_local_webhook_stop',
  });
}

/// Round-trip self-test — hit /healthz on the public ngrok URL.
/// Independent of Composio. Used by the "Test tunnel" button.
export async function openhumanComposioLocalWebhookTest(): Promise<
  CommandResponse<{ result: { ok: boolean; url: string; body: string } }>
> {
  if (!isTauri()) {
    throw new Error('Not running in Tauri');
  }
  return await callCoreRpc<
    CommandResponse<{ result: { ok: boolean; url: string; body: string } }>
  >({
    method: 'openhuman.composio_local_webhook_test',
  });
}

/// Persist the ngrok authtoken into the encrypted credential store.
/// Token is never echoed back through any RPC.
export async function openhumanComposioSetNgrokAuthtoken(
  authtoken: string
): Promise<CommandResponse<{ stored: boolean }>> {
  if (!isTauri()) {
    throw new Error('Not running in Tauri');
  }
  return await callCoreRpc<CommandResponse<{ stored: boolean }>>({
    method: 'openhuman.composio_set_ngrok_authtoken',
    params: { authtoken },
  });
}

/// Remove the stored ngrok authtoken.
export async function openhumanComposioClearNgrokAuthtoken(): Promise<
  CommandResponse<{ removed: boolean }>
> {
  if (!isTauri()) {
    throw new Error('Not running in Tauri');
  }
  return await callCoreRpc<CommandResponse<{ removed: boolean }>>({
    method: 'openhuman.composio_clear_ngrok_authtoken',
  });
}

/// Patch the non-secret webhook config fields (enabled toggle, port,
/// ngrok domain) and persist to config.toml. None of the fields are
/// required — only the ones present in the call are changed.
export async function openhumanComposioSetWebhookConfig(patch: {
  enabled?: boolean;
  port?: number;
  ngrok_domain?: string;
}): Promise<CommandResponse<{ status: ComposioLocalWebhookStatus }>> {
  if (!isTauri()) {
    throw new Error('Not running in Tauri');
  }
  return await callCoreRpc<CommandResponse<{ status: ComposioLocalWebhookStatus }>>({
    method: 'openhuman.composio_set_webhook_config',
    params: patch,
  });
}
