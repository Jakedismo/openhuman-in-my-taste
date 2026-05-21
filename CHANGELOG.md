# Changelog

Notable changes in this fork of OpenHuman (closedhuman). The fork strips
the OpenHuman product backend and the Composio OAuth aggregator, replacing
them with native OAuth, direct provider APIs, and BYO LLM API keys (or
local Ollama / LM Studio / mlx-audio). See `docs/RUN_IT_TODAY.md` for the
build runbook and `kokoro-tts-provider-installation.md` for the Kokoro
TTS reminder.

Dates are in DD.MM.YYYY.

## 19.5.2026 — Forked from OpenHuman

Forked from `tinyhumansai/openhuman` at commit `d0d9baba`. The plan locked
with Jokke: native OAuth per provider (Google + GitHub at v1), delete the
app login (single-user local desktop), Composio direct-mode for everything
the aggregator covers, LLM inference via Ollama or the user's own cloud
API key. Token storage stays on the existing on-disk-encrypted
`AuthService` (no third party in the handshake).

### Added

- **Native OAuth foundation.** PKCE primitives (RFC 7636), one-shot
  loopback redirect server (RFC 8252 §7.3), Google + GitHub authorization
  URL builders, Google token exchange + refresh client, shared
  `TokenError` across providers.
- **OAuth orchestrator** + build-time `client_id` constants; provider
  tokens persisted via the existing `AuthService`.
- **Native provider clients.** Gmail, Google Calendar, Google Drive,
  GitHub — direct API access using the user's own OAuth tokens. Shared
  bearer helper across providers.
- **Auto-refresh-on-401** in `AuthedClient` + standalone refresh
  primitive for older clients.
- **`oauth-connect` CLI binary** for end-to-end OAuth validation outside
  the GUI.
- **Slug → native-dispatch** table wired into `composio_execute`, with
  coverage for Gmail, Google Calendar, Google Drive, GitHub (including
  `GITHUB_CREATE_AN_ISSUE` and repo operations).
- **OpenAI `gpt-5.4` + medium reasoning** as the default LLM
  configuration.
- **Runbook:** `docs/RUN_IT_TODAY.md` for the local-OAuth build.

### Removed

- Billing / referral / team backend-proxy modules.
- OAuth-via-backend handoff machinery.
- Login UI and login deep-link surface (single-user local desktop).
- The privacy-sensitive Composio backend surface (stubbed).
- `OPENHUMAN_NATIVE_OAUTH` env-var gate (now always on).
- `BackendOAuthClient::validate_session_token` (unused).
- Billing menu tile from Settings.
- `sessionToken` gate on the bottom tab bar.

### Fixed

- pnpm: silenced `ERR_PNPM_IGNORED_BUILDS` for dev-only binaries; corrected
  `allowBuilds` so install passes cleanly.

## 20.5.2026 — Local-first day

Heavy day of unblockers as the fork transitioned to local-first defaults
and Composio direct-mode took over from the deleted backend proxy.

### Added

- **Composio direct mode** re-enabled for `authorize`, `execute`, `sync`,
  `get_user_profile`, `delete_connection`, `refresh_all_identities`,
  `list_triggers`, `list_available_triggers`.
- **Direct-mode trigger delivery** via an embedded ngrok webhook
  receiver: Svix-style HMAC verification, Composio v3 envelope parser,
  dispatch through the existing `trigger_triage` / `trigger_reactor`
  pipeline.
- **macOS-native `system` TTS provider** via `/usr/bin/say` +
  `afconvert` — a working out-of-the-box fallback while upstream Piper
  remained broken on macOS.
- **AI panel** accepts `ollama` and `lmstudio` as cloud-provider slugs,
  so local OpenAI-compatible runtimes can be configured through the same
  surface as cloud providers.
- **Channels hot-reload** for Telegram / Discord / iMessage listeners
  on connect / disconnect — no app restart needed after pairing a new
  account.
- **Async vault sync.** Job worker with progress callbacks, `vault_sync`
  enqueues async, new `sync_status` + `sync_all` RPCs, and a matching
  VaultPanel UI (polling, progress bar, Sync-all button).
- **Memory Workspace** shortcut to open the vault content folder in the
  OS file manager.

### Changed

- **Chat dispatch** routes through the workload factory (killed the
  dead backend gates that blocked chat in the local-OAuth fork).
- **Channel + threads providers** route through the same workload
  factory.
- **Memory-tree chat** (non-Ollama branch) routes through the workload
  factory.
- **Composio ops** (`composio_sync` / `get_user_profile` /
  `delete_connection` / `refresh_all_identities`) now use the mode-aware
  factory so direct-mode is honoured everywhere.
- **Local AI default flipped on** for fresh installs.
- **Subconscious workload** reads `subconscious_provider` instead of
  inheriting `memory_provider`.
- **Orchestrator prompt** updated to use the correct delegate tool call
  for research-style tasks.
- **Google OAuth** threads `client_secret` through token exchange +
  refresh.
- **Socket watchdog** re-attempts every 5s while disconnected.
- **Composio webhook** uses canonical v3 event types
  (`composio.trigger.message`, `composio.connected_account.expired`) and
  the v3 envelope shape.

### Fixed

- Dev: hardcoded Apple Signing Identity removed from `dev:app`.
- Home: "Reconnecting to backend" overlay removed.
- Routing: `/` now goes through `DefaultRedirect` so onboarding fires.
- `list_models` bypasses the proxy for loopback URLs and surfaces the
  URL + source in errors.
- Local AI: stopped silently rewriting user-chosen model IDs; Qwen3
  embedding family recognised as 1024-dim safe; over-eager Ollama
  runner probe (false-positive) dropped.
- Config: stopped re-injecting dead OpenHuman backend defaults into
  `config.toml`; resolved `config.toml` path correctly for the
  nested-user layout.
- Vault sync now fans out to the memory-tree ingest path; workspace
  embedder wired to user config; embed failures surfaced.
- `useUsageState` gutted — stopped spamming deleted team/billing RPCs.
- Socket connects unconditionally (chat input was blocked on missing
  session JWT).
- Chat: cosmetic reaction await bounded in `deliver_response`.
- TriggersPanel: state seeded once (not on every poll), adaptive
  polling, ungated Test button, correct RPC unwrap path, `primary-*`
  palette on Save; trigger-history hook ungated from backend session
  token.

## 21.5.2026 — Direct-mode polish + Kokoro

### Added

- **Kokoro TTS provider.** Speaks OpenAI Audio API to any local
  `/v1/audio/speech` server — mlx-audio with Kokoro (MLX-accelerated on
  Apple Silicon), kokoro-fastapi, or LM Studio's audio mode all work
  through the same provider. Settings → Voice gains endpoint / model /
  voice inputs. Installation reminder at
  `kokoro-tts-provider-installation.md`.
- **Inline trigger config form.** Static triggers with `requiredConfigKeys`
  (e.g. GitHub's `owner` + `repo`) now collect their config in-place
  from the toolkit Manage dialog — no CLI workaround needed for GitHub
  triggers.

### Changed

- **Composio direct-mode subagent** refreshes the per-toolkit catalogue
  at spawn time via the new `fetch_direct_toolkit_actions` instead of
  falling back to a cached zero-action list. Fixes "Gmail isn't
  connected" appearing for users with an active Composio Gmail
  connection.
- **Composio v3 trigger config** is parsed as JSON Schema
  (`required: string[]`); the legacy flat per-field-flag shape is kept
  as a fallback for toolkits that haven't migrated.
- **Kokoro response body** is read via a streaming accumulator (was
  `.bytes()`) with RIFF/WAVE magic sniffing, so transfer-encoded streams
  surface received bytes + content-type on failure rather than a single
  opaque `error decoding response body`.
- **Kokoro voice-id gate** positively matches `^[abefjz][mf]_…` instead
  of dropping anything containing `_` — stops ElevenLabs (`Rachel`) and
  macOS-say (`Samantha`) ids being POSTed to mlx-audio as unknown
  voices. Default model bumped to the full HF path
  `mlx-community/Kokoro-82M-bf16`.
- **Auto-updater disabled** at three layers (Tauri plugin config,
  `<AppUpdatePrompt />` mount removed from `App.tsx`, Software Updates
  card removed from the About panel). The closedhuman fork doesn't
  consume upstream tinyhumansai/openhuman releases. Re-enable path
  documented in the commit message.

### Fixed

- Composio bus: stopped dropping `ComposioTriggerReceived` events in
  direct mode (the gate that silently dropped 5 successfully-dispatched
  events).
- **Provider routing closure.** `create_backend_inference_provider`
  hard-errors with a Settings → AI pointer when no `inference_url` is
  configured (previously silently routed to OpenHumanBackendProvider →
  `SESSION_EXPIRED`). Memory-tree `CloudChatProvider` removed in favour
  of the workload factory.
- **Existing `OpenhumanJwt` rows** in `cloud_providers` are migrated to
  `Bearer` with an empty key on config load — the user re-adds their own
  API key under Settings → AI, but the misleading `SESSION_EXPIRED`
  error path is closed. Migration that minted new `OpenhumanJwt` entries
  is gone.
