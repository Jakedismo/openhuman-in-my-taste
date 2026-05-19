# Plan: Remove OpenHuman backend dependency and Composio, go local-first

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

### Phase 4 — Validate end-to-end (gate before cutover)

- [ ] **4.1** Manual: connect Google, send a test email, list 3 calendar events, write a file to Drive, list, delete. Document the consent-screen UX (unverified-app warning + verifier path).
- [ ] **4.2** Manual: connect GitHub, create an issue, list repos.
- [ ] **4.3** E2E spec: `tasks/e2e-native-oauth.spec.ts` mocking the provider's OAuth endpoints + verifying the loopback callback round-trip.
- [ ] **4.4** Rust e2e: extend `tests/json_rpc_e2e.rs` with `openhuman.oauth_*` happy + sad paths.
- [ ] **4.5** **Decision gate** — review with Jokke before Phase 5.

### Phase 5 — Hard cutover: delete Composio + app-login + dead UI

- [ ] **5.1** Delete `src/openhuman/composio/` (all 30+ files). Remove from `src/core/all.rs` registry.
- [ ] **5.2** Delete `BackendOAuthClient`, `IntegrationTokensHandoff`, `ConnectResponse`, `IntegrationSummary`, `decrypt_handoff_blob` from `src/api/rest.rs` (and the `credentials/` re-exports).
- [ ] **5.3** Delete `src/openhuman/billing/`, `src/openhuman/wallet/`, `src/openhuman/referral/`, `src/openhuman/tokenjuice/` (verified user-account-only in Phase 1.3).
- [ ] **5.4** Delete the `auth` Redux slice + every selector + every consumer that breaks. Big frontend pass: ~30–50 files touched.
- [ ] **5.5** Delete `OAuthProviderButton`, `authApi`, `apiClient`, `useBackendUrl`, `backendUrl.ts`, `Welcome` route, login-gated routes. Replace `CoreStateProvider` bootstrap with a no-auth snapshot.
- [ ] **5.6** Delete `feature.native_oauth_enabled` flag; native OAuth is the only path.
- [ ] **5.7** Verify `pnpm test`, `pnpm test:rust`, `pnpm typecheck`, `pnpm lint`, `pnpm test:e2e:all:flows` all green.

### Phase 6 — Local replacements for A3–A7

Can run in parallel with Phase 5 (or after, to keep Phase 5 a clean diff).

- [ ] **6.1** Web search (A3) — `tools/impl/network/web_search.rs` rewritten to call DuckDuckGo HTML directly (no key) OR Brave Search with BYO key from settings. Drop `WebSearchTool::client: Option<Arc<IntegrationClient>>`.
- [ ] **6.2** Apify (A4) — replace with `tools/impl/network/local_crawler.rs` using the existing CEF infrastructure for headless page fetch + extraction.
- [ ] **6.3** Twilio (A5) — `integrations/twilio.rs` rewritten as direct API; settings UI for BYO credentials.
- [ ] **6.4** Google Places (A6) — `integrations/google_places.rs` rewritten as direct API; settings UI for BYO API key.
- [ ] **6.5** Financial APIs (A7) — `integrations/stock_prices.rs` rewritten against free direct APIs (Yahoo Finance, CoinGecko, ExchangeRate-API).
- [ ] **6.6** LinkedIn enrichment (`learning/linkedin_enrichment.rs`) — either drop or fold into the webview/CDP scraping pattern.

### Phase 7 — Defaults & cleanup

- [ ] **7.1** Default LLM config → `ollama:<sensible-default>` (e.g. `ollama:llama3.1:8b` or whatever the current `inference/model_ids.rs` defaults indicate). Cloud-LLM providers stay supported but opt-in.
- [ ] **7.2** Shrink `IntegrationClient` to a tunnels-only shim (A8 stays). Rename to `TunnelClient`? Confirm with Jokke.
- [ ] **7.3** Remove `VITE_BACKEND_URL` from `app/.env.example` and `.env.example`; document the tunnels-only base URL var separately.
- [ ] **7.4** Update `CLAUDE.md` and `AGENTS.md` to reflect the new architecture (no Composio, no user accounts, native OAuth, Ollama-first).
- [ ] **7.5** Update `src/openhuman/about_app/catalog.rs` capability matrix to reflect dropped surfaces.

## Risks & open questions

- **Google "unverified app" UX** — every Gmail/Calendar/Drive consent shows a scary warning page until verification. Acceptable per scope but worth documenting in the README so a self-installer doesn't bounce off it.
- **Refresh-token expiry** — Google refresh tokens for unverified apps expire after 7 days. Production-grade solution requires verification, but for personal use a re-auth banner is fine. Documentation TODO.
- **`billing` / `wallet` / `tokenjuice` deletion blast radius** — these domains may be wired into agent prompts, capability descriptions, or routing logic in ways I haven't audited. Phase 1.3 must catch all of it before Phase 5.3 deletes them.
- **`IntegrationClient` for tunnels-only** — keeping it for A8 means `VITE_BACKEND_URL` stays meaningful. Need to confirm with Jokke if the tunnels backend is on the same origin as the old `/agent-integrations/*` backend, or a separate service.
- **Bundled Google client_id** — fine for private fork, but anyone with the binary can extract and impersonate the app. Acceptable per Jokke's "private personal fork" decision.
- **No tests today for the OAuth callback loopback** — pattern is new to this codebase; budget extra time in Phase 2.9 for harness scaffolding.

## Review

(populated at the end)
