# Phase 1 Inventory: OpenHuman Backend & Composio Removal

**Date**: 2025-05-19  
**Scope**: Read-only audit of IntegrationClient, backend URL, and user-account-driven domains ahead of Phase 5 cutover.

---

## 1. Rust IntegrationClient Consumers

All files importing or using `crate::openhuman::integrations::IntegrationClient` (the shared HTTP client for `/agent-integrations/*` backend endpoints).

### Direct Consumers (13 files)

| File | Purpose | Endpoints Called | Disposition |
|------|---------|------------------|-------------|
| `src/openhuman/composio/ops.rs:1–80` | RPC-facing Composio domain ops; wraps ComposioClient calls, error classification. Exports RPC methods for toolkits, connections, execution, triggers. | (via ComposioClient, not direct) | **DELETE** — entire composio domain goes. |
| `src/openhuman/composio/client.rs:1–2000+` | ComposioClient — HTTP wrapper for `/composio/v1/*` backend Composio proxy. Handles auth, listing, execution, triggers, provider sync, connections. | `POST /agent-integrations/composio/*` | **DELETE** — Composio removed Phase 5. |
| `src/openhuman/composio/auth_retry_tests.rs` | Unit tests for auth retry logic. | (test fixtures) | **DELETE** |
| `src/openhuman/composio/client_tests.rs` | Composio client unit tests. | (test fixtures) | **DELETE** |
| `src/openhuman/composio/execute_dispatch_tests.rs` | Execute dispatch tests. | (test fixtures) | **DELETE** |
| `src/openhuman/integrations/client.rs:1–450+` | IntegrationClient itself: shared HTTP client for all `/agent-integrations/*` routes. Manages TLS, timeouts, auth headers, error extraction. | `POST /agent-integrations/*` (Parallel, Apify, GooglePlaces, Twilio, stock prices, etc.) | **DELETE** — replaced by per-tool direct API clients in Phase 6. |
| `src/openhuman/integrations/parallel.rs:1–1000+` | Web search/extraction via Parallel backend proxy. 6 tools (Search, Extract, Chat, Research, Enrich, Dataset). | `POST /agent-integrations/parallel/{search,extract,chat,research,enrich,dataset}` | **REPLACE-DIRECT** — call Brave or DuckDuckGo directly (A3). |
| `src/openhuman/integrations/apify.rs:1–500+` | Actor execution (run, status, results). | `POST /agent-integrations/apify/{run}`, `GET /agent-integrations/apify/runs/{id}` | **REPLACE-LOCAL** — use CEF/Chromium headless crawler (A4). |
| `src/openhuman/integrations/google_places.rs:1–350+` | Place search and details. | `POST /agent-integrations/google-places/{search,details}` | **REPLACE-DIRECT** — call Google Places API directly with BYO key (A6). |
| `src/openhuman/integrations/stock_prices.rs:1–700+` | Stock quotes, options, FX, crypto, commodities. | `POST /agent-integrations/{quote,options,exchange-rate,crypto-series,commodity}` | **REPLACE-DIRECT** — direct free APIs (Yahoo, CoinGecko, ExchangeRate-API) (A7). |
| `src/openhuman/integrations/twilio.rs:1–200+` | Phone call placement. | `POST /agent-integrations/twilio/call` | **REPLACE-DIRECT** — call Twilio API directly with BYO creds (A5). |
| `src/openhuman/tools/impl/network/web_search.rs:1–300+` | WebSearchTool wrapping Parallel. | `(via IntegrationClient → Parallel)` | **REPLACE-DIRECT** — migrate to DuckDuckGo/Brave (A3). |
| `src/openhuman/learning/linkedin_enrichment.rs:1–400+` | LinkedIn profile enrichment via Gmail + Apify scraping. Calls Composio for Gmail search, then Apify for page scrape. | `(via IntegrationClient → Composio + Apify)` | **DELETE** — either drop or fold into webview scraping (phase 6.6). |
| `src/openhuman/agent/harness/test_support.rs:1–600+` | Test helper: `spawn_fake_composio_backend` axum app. Returns fixture `/agent-integrations/composio/*` responses. | `(test fixtures)` | **DELETE** |

### Test Files (4 files, all deletion targets)

| File | Purpose | Disposition |
|------|---------|-------------|
| `src/openhuman/integrations/apify_tests.rs` | Apify tests | **DELETE** |
| `src/openhuman/integrations/client_tests.rs` | IntegrationClient tests | **DELETE** |
| `src/openhuman/integrations/parallel_tests.rs` | Parallel tests | **DELETE** |

---

## 2. Backend URL / OAuth Client Consumers (Frontend)

All frontend files using `BACKEND_URL`, `getBackendUrl()`, `useBackendUrl()`, or constructing backend requests.

### Service Layer (4 files)

| File | Purpose | Disposition |
|------|---------|-------------|
| `app/src/services/backendUrl.ts:1–78` | `getBackendUrl()` resolver; fetches from `VITE_BACKEND_URL` env or core RPC `openhuman.config_resolve_api_url`. | **KEEP** — tunnels-only backend shim (A8). Rename path if needed. |
| `app/src/services/apiClient.ts:1–200+` | Generic HTTP client using `getBackendUrl()` for requests. Handles auth token injection, error parsing. | **KEEP** — base layer for tunnels API and any remaining backend calls. |
| `app/src/services/api/authApi.ts:1–64` | Email magic link + login token consumption. Calls `POST /auth/email/send-link` and `openhuman.auth.consume_login_token` RPC. | **DELETE** — no more magic-link login (A2). |
| `app/src/services/api/tunnelsApi.ts:1–100+` | Webhooks/tunnels CRUD: list, create, delete, debug logs. Calls `openhuman.webhooks_*` RPC. | **KEEP** — tunnels are A8 (out of scope). |

### API Subservices (using backendUrl indirectly, 6+ files)

| File | Purpose | Disposition |
|------|---------|-------------|
| `app/src/services/api/billingApi.ts` | Calls backend billing endpoints. | **DELETE** |
| `app/src/services/api/creditsApi.ts` | Calls backend credits/rewards endpoints. | **DELETE** |
| `app/src/services/api/referralApi.ts` | Calls backend referral endpoints. | **DELETE** |
| `app/src/services/api/rewardsApi.ts` | Calls backend rewards endpoints. | **DELETE** |
| `app/src/services/api/teamApi.ts` | Calls backend team endpoints. | **DELETE** |
| `app/src/services/api/channelConnectionsApi.ts` | Channel integrations; may call backend for connection metadata. | **REVIEW** — check if channel connections remain after cutover. |

### Components (9 files)

| File | Purpose | Disposition |
|------|---------|-------------|
| `app/src/components/oauth/OAuthProviderButton.tsx:1–100+` | Button to initiate backend OAuth flow (magic link → provider consent → handoff). | **DELETE** — replace with native OAuth (Phase 2). |
| `app/src/components/oauth/__tests__/OAuthProviderButton.test.tsx` | Tests for OAuthProviderButton. | **DELETE** |
| `app/src/components/settings/panels/BillingPanel.tsx` | Settings panel for billing/plan info. Calls `billingApi`. | **DELETE** |
| `app/src/components/settings/panels/WebhooksDebugPanel.tsx` | Debug panel for webhooks. Calls `tunnelsApi`. | **KEEP** |
| `app/src/components/webhooks/TunnelList.tsx` | UI for tunnel management. Calls `tunnelsApi`. | **KEEP** |

### Hooks (2 files)

| File | Purpose | Disposition |
|------|---------|-------------|
| `app/src/hooks/useBackendUrl.ts:1–30+` | Hook wrapping `getBackendUrl()` for component use. | **KEEP** — needed by tunnels UI. |
| `app/src/hooks/useBackendUrl.test.ts` | Tests. | **KEEP** |
| `app/src/hooks/useConsciousItems.ts` | Possibly fetches from backend. | **REVIEW** |

### Utils & Config (2 files)

| File | Purpose | Disposition |
|------|---------|-------------|
| `app/src/utils/config.ts:1–50+` | Exports `BACKEND_URL` from `VITE_BACKEND_URL` env var. | **KEEP** — needed by backendUrl.ts. Pin to tunnels-only backend. |
| `app/src/services/__tests__/backendUrl.test.ts` | Tests for backendUrl resolver. | **KEEP** |

### Pages & Routing (2 files)

| File | Purpose | Disposition |
|------|---------|-------------|
| `app/src/pages/Welcome.tsx:1–200+` | Login/registration page. Calls authApi + OAuth flow. | **DELETE** — no more login flow (A2). |
| `app/src/pages/__tests__/Welcome.test.tsx` | Tests. | **DELETE** |

### Test Setup (1 file)

| File | Purpose | Disposition |
|------|---------|-------------|
| `app/src/test/setup.ts` | Test environment config, includes backendUrl mocking. | **REVIEW** — update test mocks. |

---

## 3. User-Account-Driven Domains (Backend RPC Surface)

Each domain below is reached only via `openhuman.<namespace>.*` RPC methods that require an authenticated `auth.token` (i.e., a logged-in user) to call. Once app login is deleted (A2), these become unreachable.

### 3.1 `src/openhuman/billing/` — **DELETE-DOMAIN**

**RPC Methods** (15 total; all require auth):
- `billing_get_current_plan`
- `billing_get_balance`
- `billing_purchase_plan`
- `billing_create_portal_session`
- `billing_top_up`
- `billing_create_coinbase_charge`
- `billing_get_transactions`
- `billing_get_auto_recharge`
- `billing_update_auto_recharge`
- `billing_get_cards`
- `billing_create_setup_intent`
- `billing_update_card`
- `billing_delete_card`
- `billing_redeem_coupon`
- `billing_get_coupons`

**Backend Dependency**: All call `https://<backend>/billing/*` endpoints (Stripe proxying, coupon validation, plan inventory).

**Frontend Callers**:
- `app/src/services/api/billingApi.ts`
- `app/src/components/settings/panels/BillingPanel.tsx`
- `app/src/pages/Settings.tsx` (renders BillingPanel)
- `app/src/store/__tests__/settingsSlice.test.ts` (if billing is in Redux)

**Verdict**: **DELETE-DOMAIN** — purely user-account-driven; no local logic to preserve.

---

### 3.2 `src/openhuman/wallet/` — **DELETE-DOMAIN**

**RPC Methods** (11 total):
- `wallet.status`
- `wallet.setup`
- `wallet.balances`
- `wallet.network_defaults`
- `wallet.supported_assets`
- `wallet.encode_erc20_transfer`
- `wallet.chain_status`
- `wallet.prepare_transfer`
- `wallet.prepare_swap`
- `wallet.prepare_contract_call`
- `wallet.execute_prepared`

**Backend Dependency**: Wallet setup persisted in backend; balance fetches routed through backend API gateway to blockchain RPC providers.

**Frontend Callers**: (None found directly, but likely in Settings or agent context.)

**Verdict**: **DELETE-DOMAIN** — blockchain wallet feature; not in scope after user-account removal.

---

### 3.3 `src/openhuman/referral/` — **DELETE-DOMAIN**

**RPC Methods** (2 total):
- `referral_get_stats` — fetch referral code, link, totals, referred-user rows
- `referral_claim` — claim a referral code for the current user

**Backend Dependency**: Backend tracks referral codes, referred users, and eligibility checks (subscription status).

**Frontend Callers**:
- `app/src/services/api/referralApi.ts`
- Likely linked from Settings or a Rewards page

**Verdict**: **DELETE-DOMAIN** — purely user-account-driven; no local logic.

---

### 3.4 `src/openhuman/tokenjuice/` — **LIBRARY-ONLY (not RPC)**

**RPC Methods**: None. This domain is a library (terminal output compaction via JSON rules).

**Public Surface**:
- `reduce_execution_with_rules()`
- `load_builtin_rules()` / `load_rules()`
- `compact_tool_output()`

**Backend Dependency**: None (all local).

**Verdict**: **PARTIAL-KEEP** — library layer survives, unaffected by auth removal.

---

### 3.5 `src/openhuman/team/` — **DELETE-DOMAIN**

**RPC Methods** (15 total):
- `team_change_member_role`
- `team_create_invite`
- `team_create_team`
- `team_delete_team`
- `team_get_team`
- `team_get_usage`
- `team_join_team`
- `team_leave_team`
- `team_list_invites`
- `team_list_members`
- `team_list_teams`
- `team_remove_member`
- `team_revoke_invite`
- `team_switch_team`
- `team_update_team`

**Backend Dependency**: Backend manages user memberships, invites, usage pooling, and billing aggregation per team.

**Frontend Callers**:
- `app/src/services/api/teamApi.ts`
- `app/src/components/settings/panels/TeamPanel.tsx` (if exists)

**Verdict**: **DELETE-DOMAIN** — multi-user team collaboration; not in single-user local app scope (A2).

---

### 3.6 `src/openhuman/credentials/` — **PARTIAL-KEEP**

**Core Layer** (`core.rs`):
- `AuthService` (on-disk encrypted token store) — **KEEP**
- `AuthProfile`, `AuthProfilesStore` — **KEEP**

**Re-exports from `src/api/rest.rs`** (all deletion targets):
- `BackendOAuthClient` — **DELETE**
- `IntegrationTokensHandoff` — **DELETE**
- `decrypt_handoff_blob` — **DELETE**
- `ConnectResponse` — **DELETE**
- `IntegrationSummary` — **DELETE**

**RPC Methods**:
- `credentials_store_provider_token` — native OAuth path stores tokens here (Phase 2.6)
- `credentials_list_profiles` — list available provider profiles
- `credentials_revoke_profile` — disconnect a provider

**Verdict**: **PARTIAL-KEEP** — delete the backend OAuth handoff machinery; keep `AuthService` as the encrypted token store for native OAuth.

---

## 4. Tunnels-Only Backend Shim (A8 Scope)

Out-of-scope webhook ingress tunnels use a separate portion of the backend. Confirmation:

### Backend Routes (Tunnels)

**Confirmed RPC methods** (via `src/openhuman/webhooks/schemas.rs`):
- `webhooks.list`
- `webhooks.create`
- `webhooks.delete`
- `webhooks.debug_logs`
- `webhooks.clear_logs`
- `webhooks.list_registrations`

**Confirmed HTTP routes** (via `app/src/services/api/tunnelsApi.ts`):
- `GET /webhooks/` — list tunnels
- `POST /webhooks/` — create tunnel
- `DELETE /webhooks/{id}` — delete tunnel
- `GET /webhooks/{id}/debug` — debug logs

### Tunnels Backend Isolation

**Same origin?** Unknown (needs confirmation with Jokke per todo.md:113). The backend appears to serve both `/agent-integrations/*` (integrations, to be deleted) and `/webhooks/*` (tunnels, to survive). Need to confirm:
1. Are `/webhooks/*` and `/agent-integrations/*` on the same backend origin or separate services?
2. If same: can we delete everything except the webhooks routes and keep the backend running for A8?
3. If separate: tunnels backend lives independently; integration client deletion is clean.

### Non-Tunnels Code That Must Survive

**None identified**. All other RPC consumers (billing, team, wallet, referral, auth, Composio) are auth-gated and become unreachable once user login is removed.

### Proposed Path

1. Keep `IntegrationClient` shim or rename to `TunnelClient` (Phase 7.2).
2. Backend remains running for `/webhooks/*` routes only.
3. All integration endpoints (`/agent-integrations/*`) are deleted from backend or stubbed to 404.
4. `VITE_BACKEND_URL` pinned to tunnels-only backend endpoint.

---

## 5. Post-Cutover Diff Sketch

Rough estimates of file churn (to validate against actual PR deltas).

### Rust Files (src/ total: 1,169)

| Outcome | Count | Rationale |
|---------|-------|-----------|
| **Deleted** | ~80–100 | composio/* (28 files), integrations/(5 + 4 tests), billing/, wallet/, referral/, team/, parts of credentials/, learning/linkedin_enrichment, parts of agent/harness. |
| **Modified** | ~30–50 | Rest of credentials/, tools/, composio call sites, REST API cleanup, core registry (all.rs). |
| **New** | ~40–60 | Phase 2: oauth/ (loopback, pkce, providers/google, providers/github). Phase 3: providers_native/google/{gmail,calendar,drive}, providers_native/github. Phase 6: local_crawler, direct API clients for web_search, stock_prices, google_places, twilio. |
| **Net** | ~10–30 files + 6–8K lines removed | (Depends on how much direct API client code we add.) |

### Frontend Files (app/src/ total: 774)

| Outcome | Count | Rationale |
|---------|-------|-----------|
| **Deleted** | ~60–80 | `services/api/authApi`, `billingApi`, `creditsApi`, `referralApi`, `rewardsApi`, `teamApi`; components: OAuthProviderButton, BillingPanel, WalletPanel, ReferralPanel, TeamPanel; pages: Welcome; Redux slices: auth, (possibly billing, team, wallet). Tests for deleted components. |
| **Modified** | ~20–40 | apiClient, backendUrl (tunnels-only shim), routes (remove login guards), CoreStateProvider bootstrap. |
| **New** | ~15–25 | Phase 2 OAuth UI (OAuthFlow, provider list). |
| **Net** | ~25–40 files removed | ~3–5K lines removed. |

### Summary

- **Rust**: ~80 files deleted, 30 modified, 40–60 new → net **−40 to −50 files**, **−15 to −25K lines**.
- **Frontend**: ~70 files deleted, 30 modified, 15 new → net **−25 to −35 files**, **−3 to −5K lines**.
- **Total project**: ~100–120 files deleted; **−18 to −25K lines Rust**, **−3 to −5K lines TS**.

---

## 6. Surprises & Blockers

### None Critical

All inventory items align with scope decisions in `todo.md` (A1–A8). Confirmed:

- ✅ Composio deletable without core functionality impact.
- ✅ Billing/wallet/referral/team are purely user-account-driven; no local-only logic to preserve.
- ✅ Tokenjuice is library-only; not affected.
- ✅ Tunnels can survive independently (pending origin confirmation in Phase 1.4).
- ✅ Web search, Apify, and other integrations are proxy-only; direct replacements are straightforward (Phase 6).

### Action Items for Jokke

1. **Confirm tunnels backend origin** (todo.md:113) — same-origin with integrations or separate service?
2. **Review linkedin_enrichment fate** (todo.md:99) — drop or migrate to webview scraping?
3. **Validate channel_connections scope** — does `channelConnectionsApi` remain after user-account removal?

---

## Appendix: File Manifest

### Composio & Integrations (27 files for deletion)

```
src/openhuman/composio/
  ├── mod.rs
  ├── ops.rs
  ├── client.rs
  ├── auth_retry.rs
  ├── bus.rs
  ├── action_tool.rs
  ├── error_mapping.rs
  ├── execute_dispatch.rs
  ├── execute_prepare.rs
  ├── googlecalendar_args.rs
  ├── periodic.rs
  ├── schemas.rs
  ├── tools.rs
  ├── trigger_history.rs
  ├── types.rs
  ├── [8 test files]
  └── providers/ (24 provider-specific files)

src/openhuman/integrations/
  ├── mod.rs
  ├── client.rs
  ├── parallel.rs
  ├── apify.rs
  ├── google_places.rs
  ├── stock_prices.rs
  ├── twilio.rs
  ├── seltz.rs
  ├── types.rs
  ├── [4 test files]
```

### User-Account Domains (18 files for deletion)

```
src/openhuman/billing/
  ├── mod.rs
  ├── ops.rs
  ├── schemas.rs
  └── [test files]

src/openhuman/wallet/
  ├── mod.rs
  ├── ops.rs
  ├── schemas.rs
  └── [test files]

src/openhuman/referral/
  ├── mod.rs
  ├── ops.rs
  ├── schemas.rs

src/openhuman/team/
  ├── mod.rs
  ├── ops.rs
  ├── schemas.rs
  └── [test files]
```

### Frontend Deletions (65+ files)

```
app/src/
  ├── pages/Welcome.tsx
  ├── services/api/
  │   ├── authApi.ts
  │   ├── billingApi.ts
  │   ├── creditsApi.ts
  │   ├── referralApi.ts
  │   ├── rewardsApi.ts
  │   ├── teamApi.ts
  ├── components/oauth/OAuthProviderButton.tsx
  ├── components/settings/panels/BillingPanel.tsx
  ├── store/ (auth, billing, team Redux slices)
  └── [tests for all above]
```

---

**End of Inventory**
