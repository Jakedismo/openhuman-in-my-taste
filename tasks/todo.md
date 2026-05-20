# Plan: Remove OpenHuman backend dependency and Composio, go local-first

## Getting started (run it today)

After the local-OAuth + OpenAI-default cut, a fresh launch needs three things wired up before chat works end-to-end:

1. **Build the core + CLI**
   ```bash
   cargo build --bin openhuman-core --bin oauth-connect
   ```

2. **Store your OpenAI API key.** The `unify_ai_provider_settings` migration seeds a `cloud_providers` entry with slug `openai` on first launch; the key lives encrypted in `auth-profiles.json`:
   ```bash
   ./target/debug/openhuman-core rpc auth_store_provider_credentials \
     --params '{"provider":"openai","token":"sk-..."}'
   ```
   The factory routes every workload through `DEFAULT_MODEL = "openai:gpt-5.4"` against `https://api.openai.com/v1/responses` with `reasoning.effort = "medium"`.

3. **(Optional) Connect Gmail / Calendar / Drive / GitHub natively.** Run the loopback OAuth flow per provider — tokens land in the same `auth-profiles.json`:
   ```bash
   OPENHUMAN_GOOGLE_OAUTH_CLIENT_ID=<...> ./target/debug/oauth-connect --provider google
   OPENHUMAN_GITHUB_OAUTH_CLIENT_ID=<...> ./target/debug/oauth-connect --provider github
   ```
   The 9 wired tool slugs (`GMAIL_*`, `GOOGLECALENDAR_*`, `GOOGLEDRIVE_*`, `GITHUB_*`) then resolve through `oauth/native_dispatch.rs` and never touch any third-party broker.

4. **Run the Tauri shell** as usual: `pnpm dev:app`.

---


## Goal

Eliminate the privacy risk where the OpenHuman backend and Composio sit between the user and their OAuth tokens. After this work, OAuth tokens for connected providers are obtained directly from the provider (no third party in the handshake), stored locally in the existing on-disk-encrypted `AuthService`, and used directly from the Rust core. Local replacements for backend-proxied tools (web search, scraping, maps, finance, telephony). LLM calls routed to Ollama by default.

## Scope decisions (locked with Jokke)

| Topic | Decision |
| --- | --- |
| **A1 Composio** | Native OAuth per provider. v1 providers: **Google (Gmail + Calendar + Drive)** and **GitHub**. |
| **A2 App login** | **Delete entirely.** Single-user local desktop app. No more magic-link, no social SSO, no team/billing/wallet/referral/credits/tokenjuice user-scoped concept. |
| **A3 Web search** | Replace Parallel-via-backend with direct (DuckDuckGo HTML or Brave Search with BYO key). |
| **A4 Apify** | Replace with local CEF/headless Chromium crawler. |
| **A5 Twilio** | Direct API with user's own creds. |
| **A6 Google Places** | Direct Google Places API with user's own key. |
| **A7 Financial APIs** | Direct free APIs (Yahoo Finance, CoinGecko, ExchangeRate-API) or BYO. |
| **A8 Webhook tunnels** | **Out of scope.** `IntegrationClient` shrinks to a tunnels-only shim. |
| **LLM inference** | Already routes Ollama natively via `ollama:*` model names (`src/openhuman/inference/http/server.rs:1`). Just a default-config change. |
| **Google OAuth distribution** | Private fork: bundle single unverified Google OAuth client_id at build time. Users will see Google's "unverified app" consent warning — acceptable for private/personal use. |
| **Token storage** | Keep existing on-disk encrypted `AuthService` (`src/openhuman/credentials/core.rs:18`). Force `config.secrets.encrypt = true`. No `keyring` dependency added. |
| **Migration** | Hard cutover. No production users. |
| **Cutover order** | Build new path beside old → validate Google end-to-end → only then delete Composio + app-login + dead UI in one cut. |

## Existing infrastructure we'll reuse

- `src/openhuman/credentials/` — `AuthService`, `AuthProfilesStore`, `AuthProfile` (per-provider `provider/profile_name/token/metadata`, active-profile). This is the slot native-OAuth access/refresh tokens drop into; no new persistence layer needed.
- `src/openhuman/credentials/core.rs:18` — encrypted-at-rest secret writes already exist behind `config.secrets.encrypt`.
- `src/openhuman/inference/` — already has Ollama routing built in.
- `src/openhuman/webview_accounts/` — passive cookie heuristic (NOT a token store). Stays as-is; orthogonal to native OAuth.
- Tauri shell `core_rpc_relay` IPC pattern — for loopback redirect handling, the Rust core can listen on `http://127.0.0.1:<random>/oauth/callback` and the frontend handles "open external browser" via existing platform integration.

## What we'll delete at the end

- `src/openhuman/composio/` (entire domain)
- `src/openhuman/integrations/` proxies that don't survive A3–A7 replacement (Parallel, Apify, Twilio, GooglePlaces, FinancialAPIs — wholly or trimmed to direct-API stubs)
- Backend OAuth/auth surface: `BackendOAuthClient` re-exports, `decrypt_handoff_blob`, `IntegrationTokensHandoff`, `ConnectResponse`, `IntegrationSummary` (and the matching `src/api/rest.rs` machinery they come from, modulo what tunnels needs)
- `src/openhuman/billing/`, `src/openhuman/wallet/`, `src/openhuman/referral/`, `src/openhuman/tokenjuice/` — verify each is fully user-account-driven; if so, delete
- Frontend: `auth` Redux slice + selectors, `apiClient.ts`, `authApi.ts`, `OAuthProviderButton.tsx`, `backendUrl.ts`, `useBackendUrl.ts`, `Welcome` route, every Settings panel keyed off auth/billing
- `VITE_BACKEND_URL` env var (or pin it to the tunnels-only shim's URL until A8 is addressed)

## Phases

### Phase 1 — Inventory & deprecation map (no code changes) ✅

Full report: [`tasks/phase-1-inventory.md`](phase-1-inventory.md) (419 lines).

- [x] **1.1** Cataloged 13 direct + 4 test `IntegrationClient` consumers. All marked REPLACE-DIRECT / REPLACE-LOCAL / DELETE per scope.
- [x] **1.2** Cataloged ~30 frontend backend-URL consumers. `apiClient.ts`, `backendUrl.ts`, `tunnelsApi.ts`, `useBackendUrl` survive (tunnels). `authApi`, `billingApi`, `creditsApi`, `referralApi`, `rewardsApi`, `teamApi`, `OAuthProviderButton`, `Welcome`, billing/wallet/team panels all DELETE.
- [x] **1.3** Verified: `billing/` (15 RPCs), `wallet/` (11), `referral/` (2), `team/` (15) are fully user-account-driven → DELETE-DOMAIN each. `tokenjuice/` is library-only, untouched. `credentials/` is PARTIAL-KEEP: keep `AuthService`, delete `BackendOAuthClient`/`IntegrationTokensHandoff`/`decrypt_handoff_blob`.
- [x] **1.4** Tunnels backend confirmed as the only surviving dependency (RPCs `webhooks.{list,create,delete,debug_logs,clear_logs,list_registrations}`, HTTP routes under `/webhooks/*`). **Open question for Jokke**: same-origin as old `/agent-integrations/*` or separate service?
- [x] **1.5** Diff sketch: Rust −80 to −100 files / −15 to −25K lines · Frontend −60 to −80 files / −3 to −5K lines · Plus ~40–60 new Rust files (oauth domain + provider clients + local replacements).

**Open questions surfaced by Phase 1** (do not block Phase 2):
1. **Tunnels backend origin** — same as old integrations origin or separate service? Affects whether `VITE_BACKEND_URL` becomes a tunnels-only shim or stays as-is.
2. **LinkedIn enrichment** (`src/openhuman/learning/linkedin_enrichment.rs`) — drop entirely, or migrate to the webview/CDP scraping pattern alongside the messaging providers?
3. **`channelConnectionsApi`** — does it survive user-account removal? Frontend uses it for the cross-provider connection list; need to confirm whether the backing data is local or backend-side.

### Phase 2 — Native OAuth domain (feature-flagged, alongside Composio)

- [ ] **2.1** Create `src/openhuman/oauth/` domain following the controller-only pattern (`mod.rs`, `schemas.rs`, `ops.rs`, `loopback.rs`, `pkce.rs`, providers/`google.rs`, providers/`github.rs`).
- [x] **2.2** Loopback redirect server in `oauth/loopback.rs` — binds `127.0.0.1:0`, captures the `code` + `state` query params, hands them back to the pending flow via `oneshot`. Closes after one request. Per-flow random port. **9 tests green** (6 pure-parse branches + 3 end-to-end over reqwest including timeout).
- [x] **2.3** PKCE helpers in `oauth/pkce.rs` — `code_verifier` (32 random bytes → 43-char base64url-no-pad), `code_challenge` (SHA-256 + base64url-no-pad), `state` (16 random bytes). 9 unit tests in `pkce_tests.rs` including RFC 7636 Appendix B fixed test vector. **All green.**
- [ ] **2.4** Google provider — single OAuth client_id baked at build time via build-script env var (`OPENHUMAN_GOOGLE_OAUTH_CLIENT_ID`, with a placeholder for local dev). Scopes: `gmail.readonly`, `gmail.send`, `calendar`, `calendar.events`, `drive.file`. Refresh token plumbing.
  - [x] **2.4a** `build_auth_url(&AuthUrlParams)` + `AUTH_ENDPOINT` / `TOKEN_ENDPOINT` / `DEFAULT_SCOPES` constants. 8 tests green (param pinning, percent-encoding, custom-scope override).
  - [x] **2.4b** `GoogleClient::{exchange_code, refresh_access_token}` + `TokenResponse` + `TokenError`. 7 tests green via local axum mock token endpoint (request-form pinning, 400 raw-body surfacing, decode-error preservation, refresh-grant no-redirect-uri assertion, refresh-without-new-refresh-token tolerance).
  - [x] **2.4c** Build-time client_id discovery via `option_env!` (`OPENHUMAN_GOOGLE_OAUTH_CLIENT_ID` + `OPENHUMAN_GITHUB_OAUTH_CLIENT_ID`). Runtime guard returns typed `OAuthFlowError::ClientIdMissing { provider, env_var }` carrying the env-var name. Folded into `oauth/ops.rs`.
- [x] **2.5** GitHub provider — `build_auth_url` + `GithubClient::{exchange_code, refresh_access_token}` + `TokenResponse`. `Accept: application/json` header pinned. 200-with-error-payload trap caught. Shared `TokenError` lifted to `providers/mod.rs`. 12 tests green.
- [x] **2.6** Persistence — `oauth/persistence.rs` maps `google::TokenResponse` / `github::TokenResponse` → `credentials::TokenSet`, calls new `AuthService::store_provider_oauth_tokens(...)` (added symmetric to `store_provider_token`). 8 tests green: pure mapping (Google expiry computation, GitHub classic-no-expiry, GitHub expiring), full encrypted-disk roundtrip via real `AuthService` + `tempfile::TempDir`, multi-provider non-collision, active-profile marking, idempotent re-save.
- [x] **2.7** Orchestrator in `oauth/ops.rs` — `start_google_flow(http)` / `start_github_flow(http)` → `OAuthFlow { auth_url, redirect_uri }`. `OAuthFlow::complete(service, profile_name, timeout)` waits on the loopback, validates state (CSRF guard), exchanges the code via the provider client, and persists tokens via `AuthService`. 8 tests green covering: client-id-missing typed error, auth-url shape with live loopback, end-to-end happy path for Google + GitHub against a local mock token endpoint, state-mismatch refusal (asserts no persistence and no token-endpoint hit), loopback timeout. **RPC surface (`openhuman.oauth_start` / `_status` / `_disconnect`) deferred — the orchestrator API is already callable; RPC is a thin wrapping job that lands together with the frontend OAuth UI in a later slice.**
- [ ] **2.8** Config flag `feature.native_oauth_enabled` — defaults false; flip true when validating; eventually deleted in Phase 5.
- [x] **2.9** Unit tests — 61 total in the `oauth/` suite (9 pkce + 9 loopback + 15 google + 12 github + 8 persistence + 8 orchestrator). Refresh path covered (`refresh_access_token` + idempotent re-save); state validation covered; loopback end-to-end covered.

### Phase 3 — Provider clients (replace Composio data calls)

- [x] **3.1** `src/openhuman/providers_native/google/{gmail.rs, calendar.rs, drive.rs}` — direct HTTPS clients. Pull tokens from `AuthService` via shared `bearer::AuthedClient`. Refresh on 401 surfaced as typed error (auto-retry deferred to a later slice). Operations covered:
  - **Gmail**: `send_message`, `list_messages`, `delete_message`, `add_label`
  - **Calendar**: `list_events` (with the singleEvents=true / timeZone defaulting from issue #1714), `get_event`, `create_event`
  - **Drive**: `list_files`, `create_file_metadata`, `get_file_metadata` (under `drive.file` scope)
- [x] **3.2** `src/openhuman/providers_native/github.rs`: `get_authenticated_user`, `list_authenticated_repos`, `create_issue`.
- [x] **3.3** Replace one Composio call site at a time behind `OPENHUMAN_NATIVE_OAUTH=1`. **Part 1 done**: `src/openhuman/oauth/native_dispatch.rs` exposes `try_dispatch_native(http, service, tool, args) -> Option<Result<Value>>`. **Part 2 done**: wired into `composio/ops.rs::composio_execute` via a `try_native_dispatch` helper that short-circuits the Composio path entirely on native success, wrapping the result in `ComposioExecuteResponse` so callers and the `ComposioActionExecuted` event-bus payload see an identical shape. Full slug coverage:
  - **Gmail** — `GMAIL_SEND_EMAIL`, `GMAIL_FETCH_EMAILS`, `GMAIL_DELETE_EMAIL`, `GMAIL_ADD_LABEL_TO_EMAIL`
  - **Calendar** — `GOOGLECALENDAR_EVENTS_LIST` (+ `GOOGLECALENDAR_FIND_EVENT` alias), `GOOGLECALENDAR_EVENTS_GET`, `GOOGLECALENDAR_CREATE_EVENT`
  - **GitHub** — `GITHUB_USERS_GET_AUTHENTICATED`, `GITHUB_CREATE_AN_ISSUE`
  - **Refresh primitive** — `oauth/refresh.rs::refresh_provider_token(http, service, provider)` trades the stored refresh_token for a fresh access_token + persists. Handles Google's "no fresh refresh_token on refresh-grant" quirk and GitHub's classic/expiring split. Not yet wired into `bearer::AuthedClient` — follow-up slice will catch 401 responses and call this transparently.
  Composio's 459-test integration suite still passes with the flag off — the fall-through path is the load-bearing regression guard.
- [ ] **3.4** RPC: a proper `openhuman.oauth_*` JSON-RPC method via the controller registry — pairs with the eventual frontend OAuth UI. **Interim**: `oauth-connect` CLI binary (`src/bin/oauth_connect.rs`) drives the full Google / GitHub flow end-to-end and persists tokens locally, so Phase 4 validation is unblocked without waiting on the controller wiring. Pairs with 3.3 cutover.

#### Extended dispatch coverage (post-Phase 5.6)

Phase 5.1 made native dispatch the only execution path — every slug without a native arm hard-errors at `composio_execute`. The arms below were added to cover the agent's day-to-day toolset:

- **Gmail**: `GMAIL_SEND_EMAIL`, `GMAIL_FETCH_EMAILS`, `GMAIL_DELETE_EMAIL`, `GMAIL_ADD_LABEL_TO_EMAIL`
- **Calendar**: `GOOGLECALENDAR_EVENTS_LIST` (+ `GOOGLECALENDAR_FIND_EVENT` alias), `GOOGLECALENDAR_EVENTS_GET`, `GOOGLECALENDAR_CREATE_EVENT`
- **Drive**: `GOOGLEDRIVE_LIST_FILES` / `GOOGLEDRIVE_FIND_FILE`, `GOOGLEDRIVE_GET_FILE_METADATA`, `GOOGLEDRIVE_CREATE_FILE` / `GOOGLEDRIVE_CREATE_FILE_FROM_TEXT`
- **GitHub**: `GITHUB_USERS_GET_AUTHENTICATED`, `GITHUB_CREATE_AN_ISSUE`, `GITHUB_LIST_REPOSITORIES_FOR_THE_AUTHENTICATED_USER`

### Phase 4 — Validate end-to-end (gate before cutover)

- [ ] **4.1** Manual: connect Google, send a test email, list 3 calendar events, write a file to Drive, list, delete. Document the consent-screen UX (unverified-app warning + verifier path).
- [ ] **4.2** Manual: connect GitHub, create an issue, list repos.
- [ ] **4.3** E2E spec: `tasks/e2e-native-oauth.spec.ts` mocking the provider's OAuth endpoints + verifying the loopback callback round-trip.
- [ ] **4.4** Rust e2e: extend `tests/json_rpc_e2e.rs` with `openhuman.oauth_*` happy + sad paths.
- [ ] **4.5** **Decision gate** — review with Jokke before Phase 5.

### Phase 5 — Hard cutover: delete Composio + app-login + dead UI

- [x] **5.1** Composio: **stub** rather than delete, per user "stub-and-delete" choice.
  - `composio_execute` now hard-errors when no native arm exists (was: fell through to Composio HTTP).
  - `composio_authorize` now hard-errors with pointer to native flow (was: created backend OAuth handoff).
  - `fetch_connected_integrations_uncached` returns `None` unconditionally — the agent prompt sees zero connections until a follow-up re-sources from `AuthService`.
  - `start_periodic_sync` is a no-op.
  - HTTP-calling internals (`ComposioClient::*`, provider outbound fetches) are still compiled but unreachable. Slack-memory ingest controllers (local data) stay registered.
  - 3 mock-backend tests removed (paths gone); 2 "errors_without_session" tests rewritten to assert the new disabled-state messages.
- [x] **5.2** Deleted `IntegrationTokensHandoff`, `ConnectResponse`, `IntegrationSummary`, `decrypt_handoff_blob` + supporting envelope structs from `src/api/rest.rs`. Removed `connect`/`list_integrations`/`fetch_integration_tokens_handoff`/`fetch_client_key`/`revoke_integration` methods from `BackendOAuthClient`. Dropped the 5 `auth_oauth_*` RPCs from `credentials/`. `BackendOAuthClient` itself stays for surviving non-OAuth backend calls (channels, voice, login) — those go in 5.4 / Phase 6.
- [x] **5.3** Deleted `src/openhuman/billing/`, `src/openhuman/referral/`, `src/openhuman/team/` (all confirmed `BackendOAuthClient`-tied). **Kept `src/openhuman/wallet/`** — Phase 1.3 mis-classified it; it's a local crypto wallet (balances/transfers/swaps) with zero backend dependency. `tokenjuice/` was always library-only and remains untouched.
- [x] **5.4** Frontend login-surface cut (two slices):
  - **Slice 1**: deleted `OAuthProviderButton`, `OAuthLoginSection`, `providerConfigs`, `Welcome.tsx`, `PublicRoute.tsx`. `AppRoutes` now redirects `/` → `/home` and every `<ProtectedRoute>` drops `requireAuth`. `ProtectedRoute` simplified to a bootstrap-passthrough; `DefaultRedirect` no longer routes-on-no-token.
  - **Slice 2**: deleted `desktopDeepLinkListener.ts` (login/Composio-OAuth/Stripe deep-link handlers — all dead), `authApi.ts` (magic-link + consumeLoginToken), `deepLinkAuthState.ts` (no surviving readers). `main.tsx` drops `setStoreForApiClient` wiring and `setupDesktopDeepLinkListener()` boot call.
  - **Deferred**: `apiClient.ts` (still used by `mascotService`, `meetCallService`, `rewardsApi`, `inviteApi` — Phase 6 work); `backendUrl.ts` + `useBackendUrl.ts` (webhooks tunnel still needs these — A8 keeps); `sessionToken` field on `CoreStateProvider` (15+ null-tolerant consumers; sweep is mechanical but touches a lot of files).
- [ ] **5.5** (merged into 5.4 above — the Redux auth slice never existed in this codebase; the pattern was already migrated to `CoreStateProvider` before this work began.)
- [x] **5.6** Dropped the `OPENHUMAN_NATIVE_OAUTH` env-var gate on `try_dispatch_native`. Native dispatch is unconditional now — partial-rollout scaffolding gone. Outdated test note + 3 obsolete routing regression tests removed.
- [ ] **5.7** Verify `pnpm test`, `pnpm test:rust`, `pnpm typecheck`, `pnpm lint`, `pnpm test:e2e:all:flows` all green. **Status**: `cargo test --lib` runs 7,579 green. Frontend `pnpm compile` blocked locally on `app/node_modules` not being installed in this session; needs a fresh `pnpm install` to verify. E2E + lint still pending.

### Phase 5.5 — Post-cutover stabilisation log

Not part of the original phase ladder. Tracks fixes that surfaced **after**
Phase 5 landed, while the backend-free build was being shaken down in
day-to-day use. Each bullet is a green commit on
`feat/local-oauth-no-backend`; the SHAs are stable references for
post-mortem grepping. Grouped thematically (commit-time order within each
group).

#### Dev-env / packaging

- [x] **5.5.1** Fix `tauri:ensure` invocation runbook docs (`5588242e`).
- [x] **5.5.2** Silence `ERR_PNPM_IGNORED_BUILDS` for dev-only binaries (`7dc6b73f`).
- [x] **5.5.3** Fix `allowBuilds` entries so `pnpm install` passes (`8df718b2`).
- [x] **5.5.4** Drop hard-coded Apple Signing Identity from `dev:app` (`fa2a55ef`).
- [x] **5.5.5** `cargo fmt` collapse single-arm match in `native_dispatch` (`6edc3444`).

#### Routes / UI dead surfaces

- [x] **5.5.6** Route `/` through `DefaultRedirect` so onboarding fires (`ffa026aa`).
- [x] **5.5.7** Drop dead "Reconnecting to backend" overlay; flip local-ai default to enabled (`2f7fbe2e`).
- [x] **5.5.8** Gut `useUsageState` — stop spamming deleted team/billing RPCs (`0776d308`).
- [x] **5.5.9** Open vault content folder via OS file manager (Obsidian "Unable to find vault" workaround, `c50bcbd3`).

#### Local AI / Ollama posture

- [x] **5.5.10** Stop silently rewriting user-chosen model IDs (`34c9f993`).
- [x] **5.5.11** Allow `ollama` / `lmstudio` as cloud-provider slugs in AI panel (`36034cd6`).
- [x] **5.5.12** Recognise Qwen3 embedding family as 1024-dim safe (`9d50bb29`).
- [x] **5.5.13** Drop over-eager Ollama runner probe (false positive, `e3d20500`).
- [x] **5.5.14** Bypass proxy for loopback URLs in `list_models`; surface URL/source in errors (`88aa2910`).
- [x] **5.5.15** Stop re-injecting dead OpenHuman backend defaults into `config.toml` (`a0b931c5`).

#### Chat / inference

- [x] **5.5.16** Unblock chat in local-OAuth fork by killing dead backend gates (`24e3d6f7`).
- [x] **5.5.17** Route channel + threads providers through workload factory (`6effa0d5`).
- [x] **5.5.18** Route memory-tree chat through workload factory in non-Ollama mode (`0274a09c`).
- [x] **5.5.19** Connect socket unconditionally — chat input was blocked on missing session JWT (`ec2f977b`).
- [x] **5.5.20** Socket watchdog: re-attempt connect every 5s while `disconnected` (closes the >5s cold-restart hole the built-in `reconnectionAttempts: 5` × 1s leaves, `da7557d3`).
- [x] **5.5.21** Bound cosmetic emoji-reaction await in `deliver_response` (closes the chat-stuck-at-"Thinking… (1)" hang when local Ollama is saturated by background vault sync, `b3f35bfe`).

#### Memory / vault

- [x] **5.5.22** Wire workspace embedder to user config + surface embed failures (`9781500e`).
- [x] **5.5.23** Resolve `config.toml` path correctly for nested-user layout (`b2a1f78c`).
- [x] **5.5.24** Fan vault sync out to memory-tree ingest path (chunks visible in tree visualisation, `402184de`).
- [x] **5.5.25** Vault sync: async job worker + progress callback (`55a53544`).
- [x] **5.5.26** Vault sync RPC: enqueue async + add `sync_status` + `sync_all` (`b8e32ec0`).
- [x] **5.5.27** VaultPanel UI: polling + progress bar + Sync All button (`2cf30943`).

#### OAuth / Composio direct mode

- [x] **5.5.28** Thread Google `client_secret` through token exchange + refresh (`0789a121`).
- [x] **5.5.29** Re-enable Composio direct mode for `authorize` / `execute` / `sync` + periodic loop (`f6b0012b`).
- [x] **5.5.30** Route `composio_sync` through mode-aware factory (`021f755b`).
- [x] **5.5.31** Route `composio_get_user_profile` + `composio_delete_connection` through mode-aware factory; new `direct_delete_connection` helper + `ComposioTool::delete_connected_account` (`9f285ec9`).
- [x] **5.5.32** Route `composio_refresh_all_identities` through mode-aware factory — closes the last `resolve_client(config)?` gate on the public ops surface (`398612d5`).

#### Channels

- [x] **5.5.33** Hot-reload telegram / discord / imessage listeners on connect/disconnect (`b3400ece`).

#### Mascot / voice

- [x] **5.5.34** macOS-native `"system"` TTS provider via `/usr/bin/say` + `/usr/bin/afconvert` (works around upstream Piper macOS release shipping the binary without its dylib chain, `24dff5f1`).

### Phase 6 — Local replacements for A3–A7 (opted out)

**Status: opted out.** The fork's actual goal was to eliminate the
**OpenHuman backend** as a privacy / availability intermediary — that's
done. The user explicitly decided online usage with **their own API
keys** against the first-party provider endpoints is fine: the keys
are user-controlled, no third party sits in the handshake, and the
data flow is the same as any other "BYO key" desktop app. Keeping the
existing direct-API call paths through `IntegrationClient` is
acceptable.

None of the items below are blocking; revisit only if a specific
provider's TOS, rate limits, or pricing becomes a problem.

- [ ] ~~**6.1** Web search (A3) — `tools/impl/network/web_search.rs` rewritten to call DuckDuckGo HTML directly (no key) OR Brave Search with BYO key from settings. Drop `WebSearchTool::client: Option<Arc<IntegrationClient>>`.~~ *(opted out — direct API with BYO key is fine)*
- [ ] ~~**6.2** Apify (A4) — replace with `tools/impl/network/local_crawler.rs` using the existing CEF infrastructure for headless page fetch + extraction.~~ *(opted out — direct API with BYO key is fine)*
- [ ] ~~**6.3** Twilio (A5) — `integrations/twilio.rs` rewritten as direct API; settings UI for BYO credentials.~~ *(opted out — direct API with BYO key is fine)*
- [ ] ~~**6.4** Google Places (A6) — `integrations/google_places.rs` rewritten as direct API; settings UI for BYO API key.~~ *(opted out — direct API with BYO key is fine)*
- [ ] ~~**6.5** Financial APIs (A7) — `integrations/stock_prices.rs` rewritten against free direct APIs (Yahoo Finance, CoinGecko, ExchangeRate-API).~~ *(opted out — direct API with BYO key is fine)*
- [ ] ~~**6.6** LinkedIn enrichment (`learning/linkedin_enrichment.rs`) — either drop or fold into the webview/CDP scraping pattern.~~ *(opted out)*

### Phase 7 — Defaults & cleanup (opted out)

**Status: opted out**, same reasoning as Phase 6. The local-first
posture became "online usage with the user's own API keys is the
default" rather than "force everything onto Ollama". 7.1's
ollama-default pivot in particular is the wrong default for the
actual decision — `config.toml` already defaults sanely
(`embeddings_provider = "ollama"`, `bge-m3`, local-ai-enabled per
`a0b931c5` + `9781500e`) and the user picks their LLM provider
explicitly via Settings → AI. The remaining cleanup items below are
still useful hygiene but are not blocking the fork's stated goal.

- [ ] ~~**7.1** Default LLM config → `ollama:<sensible-default>`…~~ *(opted out — user-picked LLM provider is the new default model; embedder + memory-extraction defaults already point at local)*
- [ ] ~~**7.2** Shrink `IntegrationClient` to a tunnels-only shim (A8 stays).~~ *(opted out — `IntegrationClient` still fronts the surviving direct-API tools per Phase 6 opt-out)*
- [ ] ~~**7.3** Remove `VITE_BACKEND_URL` from `app/.env.example` and `.env.example`; document the tunnels-only base URL var separately.~~ *(opted out — tunnels + direct-API tools still rely on it)*
- [ ] ~~**7.4** Update `CLAUDE.md` and `AGENTS.md` to reflect the new architecture (no Composio, no user accounts, native OAuth, Ollama-first).~~ *(opted out — revisit if onboarding new contributors. Today both files are accurate enough about the live state to be useful for AI agents working in the tree)*
- [ ] ~~**7.5** Update `src/openhuman/about_app/catalog.rs` capability matrix to reflect dropped surfaces.~~ *(opted out — capability rows still match what's wired)*

## Deferred / Future work

### LLM-driven namespace-graph entity extraction

Today `UnifiedMemory::upsert_document` (`src/openhuman/memory/store/unified/documents.rs`) runs **chunking + embedding** end-to-end with the user's configured embedder, but **entity / relation extraction** for the namespace knowledge graph is still hard-coded to the heuristic regex path in `src/openhuman/memory/ingestion/parse.rs::parse_document` — the `DEFAULT_MEMORY_EXTRACTION_MODEL = "heuristic-only"` label reported in logs. Patterns are tuned for chat / email / structured prose (`From:`/`To:`/`Subject:` headers, `# markdown headings`, capitalised names), so on arbitrary vault HTML / prose / source code the namespace graph stays sparse.

The memory **tree** path (`src/openhuman/memory/tree/score/extract/llm.rs::LlmEntityExtractor`) already runs an LLM against `memory_tree.llm_extractor_model` (`gemma4:e4b` in the user's config) and produces rich extraction asynchronously via the extract-job worker — but that output lands in `mem_tree_*` tables and feeds the tree visualisation + drill-down retrieval, NOT the namespace `(subject, predicate, object)` graph that `graph_query_namespace` / `query_namespace` read from.

To upgrade `UnifiedMemory::upsert_document` to LLM-driven graph extraction:

1. **New extractor module** under `src/openhuman/memory/ingestion/` (e.g. `llm_extract.rs`) — mirrors the shape of `tree::score::extract::llm::LlmEntityExtractor` but emits the namespace-graph `(RawEntity, RawRelation)` shapes consumed by `parse::parse_document` / `ExtractionAccumulator`. Lift the prompt template from the tree extractor for parity; both surfaces want the same JSON entity/relation envelope.
2. **Route via the workload factory.** `provider_for_role("memory", config)` already returns the user's configured memory workload provider (`ollama:gemma4:e4b` / `openai:gpt-5.4-mini` / etc). Build a `Box<dyn Provider>` per ingest and call its `chat_with_system` with the extraction prompt + the chunk body. Reuse the `WorkloadChatProvider` adapter from `memory/tree/chat/workload.rs` so the timing / retry behaviour matches the tree path.
3. **Async, not inline.** `upsert_document` is called from the vault sync hot path (`vault::sync::sync_vault`), the chat archivist, and Gmail / Drive ingest — running an LLM call per chunk inline would block all of them. Enqueue an `extract_namespace_graph` job (mirror the existing tree-side `extract_chunk` jobs) and have the background ingestion worker (`memory::ingestion::queue`) drain it. The doc rows + chunks land synchronously; graph relations fill in asynchronously.
4. **Soft-fallback contract.** When the LLM is unreachable / times out / parse fails, fall back to the existing heuristic path for that document so ingest stays write-through. Log a `[memory:ingestion] LLM extraction failed; falling back to heuristic` warning with the doc id so operators can spot a misconfig.
5. **Config knob.** Add `[memory] graph_extraction = "heuristic" | "llm" | "auto"` (default `auto` → LLM when `memory_provider` is set, heuristic when not). Honours the existing `memory_provider` workload field; no new top-level setting.
6. **Reporting.** Update `MemoryIngestionResult.extraction_mode` and `MemoryIngestionResult.model_name` so the UI / activity log can show "extracted via gemma4:e4b" instead of the cosmetic `heuristic-only` label.
7. **Tests.** Mock-provider tests in `memory::ingestion::tests` asserting (a) heuristic path runs when no provider configured, (b) LLM path runs with a configured `memory_provider`, (c) LLM failure falls back to heuristic, (d) extracted entities / relations land in the namespace graph (`graph_query_namespace` returns them).

**Do this after** the Phase 6 local replacements + Phase 7 defaults settle — chat / memory tree / channels are all wired now, but the namespace graph isn't on the critical path for everyday retrieval (`query_namespace` reads chunks via vector search, not the graph). The tree extractor already covers the "show me entities in this doc" UX through the Intelligence drill-down, so users have a working entity surface today; this entry is about making `graph_query_namespace` first-class for callers (agent prompts, knowledge graph viz) that want explicit subject/predicate/object queries.

### Composio direct-mode triggers (Option D) — shipped 2026-05-20

**Status (2026-05-20):** Fully shipped end-to-end via the local webhook receiver. ngrok + Axum + HMAC + bus dispatch lands in this branch. Trigger reads stayed on the mode-aware factory from `26af2474`; writes now also go through it, with the receiver bringing up a public URL that gets registered as a Composio webhook subscription on first enable.

What landed:

* **HMAC verifier** (`src/openhuman/composio/webhook_receiver/hmac.rs`): Svix-style signature check (`webhook-id` / `webhook-timestamp` / `webhook-signature` headers, HMAC-SHA256 over `{id}.{timestamp}.{body}`, constant-time compare via `subtle::ConstantTimeEq`, 5-minute timestamp tolerance). 12 known-vector tests.
* **Webhook server** (`webhook_receiver/server.rs`): Axum router with POST `/webhook` (HMAC-verify → parse `ComposioTriggerEvent` → `publish_global(DomainEvent::ComposioTriggerReceived)`) and GET `/healthz` (no-auth probe for tunnel testing). 7 integration tests including a real bus-dispatch round-trip.
* **ngrok tunnel** (`webhook_receiver/tunnel.rs`): wraps the ngrok agent SDK in-process; connects with the user's authtoken, forwards the static `<id>.ngrok-free.dev` domain to the loopback listener. `TunnelState { Idle, Connecting, Ready, Error }` for status RPC consumption.
* **Subscription helper** (`webhook_receiver/subscription.rs`): `ensure_subscription` — checks AuthService for stored subscription id+secret, creates one via Composio v3 `/webhook_subscriptions` if absent, PATCHes `enabled_events` when a new trigger's event type isn't yet covered. Returns `(ResolvedSubscription, EnsureOutcome { Created, ReusedExisting, Patched })`.
* **Lifecycle façade** (`webhook_receiver/mod.rs`): `init(config)` gates on `local_receiver_enabled` + `ngrok_domain` + authtoken; `stop()` aborts both; `public_webhook_url()` + `tunnel_state()` for the status RPC.
* **v3 client extensions** (`tools/impl/network/composio.rs`): 7 new methods — webhook subscription CRUD (`create_webhook_subscription_v3`, `get_webhook_subscription_v3`, `update_webhook_subscription_v3`, `delete_webhook_subscription_v3`) + trigger writes (`upsert_trigger_instance_v3`, `manage_trigger_instance_v3`, `delete_trigger_instance_v3`).
* **Direct-mode trigger writes** (`composio/client.rs`): `direct_enable_trigger`, `direct_disable_trigger`, `direct_create_trigger` mirror the existing read-side direct helpers.
* **Op rewiring** (`composio/ops.rs`): `composio_enable_trigger` / `composio_disable_trigger` / `composio_create_trigger` go through `create_composio_client` + `ComposioClientKind` matching just like the rest of the migrated ops. The Direct arm calls `ensure_subscription_for_trigger_write` first so the webhook URL is registered before Composio fires anything; subscription ID is persisted into `config.composio.webhook.composio_webhook_subscription_id` on the create path. Removed the now-dead `resolve_client_for_trigger_writes` gate.
* **New RPC ops**: `composio_local_webhook_status` / `_start` / `_stop` / `_test` / `set_ngrok_authtoken` / `clear_ngrok_authtoken` / `set_webhook_config`. All registered in `composio/schemas.rs`.
* **Config schema** (`config/schema/tools.rs`): `ComposioWebhookConfig` (`local_receiver_enabled`, `local_receiver_port`, `ngrok_domain`, `composio_webhook_subscription_id`) nested under `ComposioConfig`. Secrets stay in `AuthService` under new provider keys `NGROK_AUTHTOKEN_PROVIDER` and `COMPOSIO_WEBHOOK_SECRET_PROVIDER`.
* **Credentials ops** (`credentials/ops.rs`): `store_ngrok_authtoken`, `get_ngrok_authtoken`, `clear_ngrok_authtoken`; `store_composio_webhook_secret`, `get_composio_webhook_secret`, `clear_composio_webhook_secret`. Tokens never echoed back through RPC.
* **Boot lifecycle** (`channels/runtime/startup.rs`): receiver `init` is called after the bus + subscribers are wired but before message dispatch. Non-fatal — logged and swallowed if ngrok is unreachable.
* **Frontend** (`app/src/components/settings/panels/TriggersPanel.tsx`): new Settings → Triggers panel. Form for authtoken + static domain + enabled toggle + advanced port. Status block with tunnel state, public URL, subscription ID, error message (when present), bandwidth note. "Test tunnel" button hits `/healthz` via the public URL. Token write-only — never returned. Wired into `Settings.tsx`, `DeveloperOptionsPanel.tsx`, `useSettingsNavigation.ts`.
* **Dependency**: `ngrok = "0.18"` with `axum` feature. Cross-platform, pure Rust. `subtle = "2"` promoted to a direct dep for the HMAC constant-time compare.

What's still out of scope (won't be done in this fork):

* History-replay sync — events fired while the receiver was down are lost. Composio retries on a backoff for ~5 min; beyond that, dropped. Personal-use trade-off vs. additional state machinery.
* Cloudflare Tunnel alternative — would need a `TunnelProvider` trait + cloudflared subprocess management. Re-evaluate if a user actively asks for it.
* `composio_list_github_repos` direct-mode migration — separate from triggers. Still legitimately backend-only because direct mode has no equivalent for per-connection GitHub repo enumeration; would need a separate direct-mode GitHub API client.

**Verification (manual end-to-end) — what Jokke needs to do:**

1. Settings → Triggers → paste ngrok authtoken + static `<id>.ngrok-free.dev` domain → check "Enable local webhook receiver" → Save.
2. Confirm tunnel state goes Idle → Connecting → Ready within ~5 seconds.
3. Click "Test tunnel" → expect "Round-trip OK" message.
4. Open the per-toolkit Manage dialog (gmail) → enable a trigger → verify the trigger toggles on without the "receiver not running" error.
5. Trigger an actual event (e.g. send yourself an email if you enabled `GMAIL_NEW_GMAIL_MESSAGE`) → verify a `[composio-webhook] dispatching verified trigger to event bus` log line within seconds → verify the existing `trigger_triage` agent picks it up downstream.
6. Optional: kill the app → restart → confirm subscription ID persists in `config.toml`, secret persists in `AuthService`, tunnel reconnects on the same domain.

---

### Composio direct-mode triggers (Option D) — earlier interim state

**Status (2026-05-20, commit `26af2474`):** Read paths migrated, write paths intentionally still gated. Interim pre-work from the original entry is **done**: read ops surface real catalog data instead of empty / misleading-error stubs, and writes surface a clear "needs backend webhook receiver" gate.

What landed in `26af2474`:

* `composio_list_triggers` and `composio_list_available_triggers` route through `create_composio_client(config)` + `ComposioClientKind::{Backend, Direct}` matching the rest of the migrated ops.
* New v3 helpers in `src/openhuman/tools/impl/network/composio.rs`: `list_active_triggers_v3` (GET `/api/v3/trigger_instances/active`) and `list_trigger_types_v3` (GET `/api/v3/triggers_types?toolkit_slugs=…`).
* New mapper helpers in `src/openhuman/composio/client.rs`: `direct_list_active_triggers` (reshapes v3 trigger instances into canonical `ComposioActiveTriggersResponse`, derives `toolkit` from slug prefix, maps `disabled_at` → `state`) and `direct_list_available_triggers` (reshapes v3 trigger types into canonical `ComposioAvailableTriggersResponse`, extracts `required_config_keys` from the v3 `config` JSON-Schema-style descriptor).
* New `resolve_client_for_trigger_writes` gate on `composio_enable_trigger` / `composio_disable_trigger` / `composio_create_trigger`: the error explains the *real* constraint ("requires the OpenHuman backend to receive Composio's HMAC-verified webhook deliveries") instead of the misleading "Sign in first (auth_store_session)".

What's left for full direct-mode trigger writes (still deferred):

1. **Local webhook receiver** — Composio delivers trigger events to a public HTTPS URL. This is the structural blocker: in direct mode there is no OpenHuman backend fronting the receiver. Needs either ngrok / cloudflare-tunnel auto-provisioning, a relay tied to the existing A8 tunnels surface, or a user-supplied public URL.
2. **HMAC verification** of inbound webhook payloads using the per-user webhook secret from app.composio.dev.
3. **Webhook → event_bus dispatch** so the existing `trigger_triage` / `trigger_reactor` agents pick up the event.
4. **Direct-mode write helpers** in `src/openhuman/composio/client.rs` — `direct_enable_trigger`, `direct_disable_trigger`, `direct_create_trigger` (hits `PATCH /api/v3/trigger_instances/manage/{triggerId}`, `POST /api/v3/trigger_instances/{slug}/upsert`). Cheap once the webhook receiver lands.
5. **Op rewiring** — swap `resolve_client_for_trigger_writes(config)?` for the same factory match as the reads.

`composio_list_github_repos` still uses the original `resolve_client` gate (legitimately backend-only — direct mode has no equivalent for the per-connection GitHub repo enumeration; would need a separate direct-mode GitHub API client).

**Do the remaining work after** the rest of the local-first goals settle. The read-side fix in `26af2474` is enough to unblock the trigger UI for direct-mode users in browse/inspect mode; writes will surface the new clearer error.

## Risks & open questions

- **Google "unverified app" UX** — every Gmail/Calendar/Drive consent shows a scary warning page until verification. Acceptable per scope but worth documenting in the README so a self-installer doesn't bounce off it.
- **Refresh-token expiry** — Google refresh tokens for unverified apps expire after 7 days. Production-grade solution requires verification, but for personal use a re-auth banner is fine. Documentation TODO.
- **`billing` / `wallet` / `tokenjuice` deletion blast radius** — these domains may be wired into agent prompts, capability descriptions, or routing logic in ways I haven't audited. Phase 1.3 must catch all of it before Phase 5.3 deletes them.
- **`IntegrationClient` for tunnels-only** — keeping it for A8 means `VITE_BACKEND_URL` stays meaningful. Need to confirm with Jokke if the tunnels backend is on the same origin as the old `/agent-integrations/*` backend, or a separate service.
- **Bundled Google client_id** — fine for private fork, but anyone with the binary can extract and impersonate the app. Acceptable per Jokke's "private personal fork" decision.
- **No tests today for the OAuth callback loopback** — pattern is new to this codebase; budget extra time in Phase 2.9 for harness scaffolding.

## Review

(populated at the end)
