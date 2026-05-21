# OpenHuman (closedhuman fork)

**Personal AI desktop — React + Tauri v2 with a Rust core (JSON-RPC / CLI).**

Forked from [`tinyhumansai/openhuman`](https://github.com/tinyhumansai/openhuman) at commit `d0d9baba` on 19.5.2026. The fork strips the OpenHuman product backend and the Composio OAuth aggregator and replaces them with native OAuth, direct provider APIs, and BYO LLM API keys (or local Ollama / LM Studio / mlx-audio). Single-user local desktop — there is no app login, no team / billing / referral surface, and no cloud session.

| Doc | Purpose |
| --- | --- |
| [`CHANGELOG.md`](CHANGELOG.md) | What shipped, by date |
| [`docs/RUN_IT_TODAY.md`](docs/RUN_IT_TODAY.md) | Build + first-run runbook for the local-OAuth fork |
| [`kokoro-tts-provider-installation.md`](kokoro-tts-provider-installation.md) | mlx-audio + Kokoro install reminder |
| [`AGENTS.md`](AGENTS.md) | RPC controller pattern, `RpcOutcome<T>` contract |
| [`tasks/todo.md`](tasks/todo.md) | Live plan / phase log |
| [`gitbooks/developing/architecture.md`](gitbooks/developing/architecture.md) | Narrative architecture |
| [`gitbooks/developing/architecture/frontend.md`](gitbooks/developing/architecture/frontend.md) | Frontend layout |
| [`gitbooks/developing/architecture/tauri-shell.md`](gitbooks/developing/architecture/tauri-shell.md) | Tauri shell |
| [`gitbooks/developing/architecture/agent-harness.md`](gitbooks/developing/architecture/agent-harness.md) | Agent harness / tool surface |

---

## Repository layout

| Path | Role |
| --- | --- |
| **`app/`** | pnpm workspace `openhuman-app` (check `app/package.json` for the live version): Vite + React (`app/src/`), Tauri desktop host (`app/src-tauri/`), Vitest tests |
| **`src/`** (root) | Rust lib crate `openhuman`. Transport layers: `src/core/` (in-process JSON-RPC server, CLI, dispatch, auth, event bus), `src/api/` (Axum REST + Socket.IO + JWT — now mostly internal-loopback only), `src/rpc/` (RPC dispatch + structured errors). Domain logic in `src/openhuman/*`. Entry point `src/main.rs` produces the `openhuman-core` binary. |
| **`Cargo.toml`** (root) | Core crate. `cargo build --bin openhuman-core` builds the CLI. Helper binaries in `src/bin/`: `slack-backfill`, `gmail-backfill-3d`, `inference-probe`, **`oauth-connect`** (end-to-end native-OAuth validation). |
| **`packages/`** | Distribution packaging targets: `deb/`, `homebrew/`, `homebrew-core/`, `npm/`. Touched when cutting releases. |
| **`remotion/`** | Remotion project for rendering mascot runtime assets (`pnpm mascot:render`). |
| **`tests/`** | Root-level cargo tests including `tests/json_rpc_e2e.rs` (full RPC E2E exercised by `pnpm test:rust`). |
| **`e2e/`** | Docker harness for running the Linux E2E suite from macOS (`docker compose -f e2e/docker-compose.yml run --rm e2e`). |
| **`docs/`** | Deep internals + the [`RUN_IT_TODAY.md`](docs/RUN_IT_TODAY.md) runbook. Public contributor docs live in `gitbooks/developing/`. |
| **`.claude/`** | Agent-harness config: `agents/`, `commands/`, `rules/`, `mcp.json`, `settings.json`, and **[`memory.md`](.claude/memory.md)** — project-specific fixes, gotchas, and workflow notes. Read at session start. |

Commands assume the **repo root**; `pnpm dev` delegates to the `app` workspace. The root `package.json` is `openhuman-repo` (private) and enforces pnpm via the `packageManager` field.

---

## Runtime scope

- **Shipped product**: desktop — Windows, macOS, Linux. Single-user local install.
- **Tauri host** (`app/src-tauri`): desktop-only. No Android/iOS branches.
- **Core runs in-process** inside the Tauri host as a tokio task — there is **no sidecar binary anymore** (removed in upstream PR #1061). The lifecycle is owned by `core_process::CoreProcessHandle` in `app/src-tauri/src/core_process.rs`; on Cmd+Q the core dies with the GUI. Frontend RPC still goes over HTTP (`core_rpc_relay` + `core_rpc` client) to `http://127.0.0.1:<port>/rpc`, authenticated with a per-launch bearer in `OPENHUMAN_CORE_TOKEN`. Set `OPENHUMAN_CORE_REUSE_EXISTING=1` to attach to an externally-started `openhuman-core` process (e.g. a debug harness).
- **No OpenHuman product backend.** The fork doesn't connect to `api.openhuman.ai` for anything — auth, billing, voice, OAuth handoff, LLM inference. Every workload that used to proxy through it now either runs locally (Ollama / LM Studio / Kokoro on mlx-audio) or against the user's own cloud API key configured under Settings → AI.
- **Auto-updater disabled.** `tauri-plugin-updater` is in deps but `tauri.conf.json` has `updater.active: false`; the `<AppUpdatePrompt />` mount is removed from `App.tsx`. The closedhuman fork doesn't consume upstream release feeds. Re-enable path documented in commit `7ebfc39a`.

**Where logic lives**

- **Rust core**: business logic, execution, domains, RPC, persistence, CLI. Authoritative.
- **Tauri + React (`app/`)**: UX, screens, navigation, bridging to the core. Presents and orchestrates only.

---

## Auth + identity model

The fork is single-user local. There is no "app session", no JWT to OpenHuman, no `/login` route.

- **Native OAuth** via PKCE (RFC 7636) + a one-shot loopback redirect server (RFC 8252 §7.3). Currently wired for **Google** (Gmail / Calendar / Drive) and **GitHub**. Code under `src/openhuman/oauth/`. Build-time `client_id` constants; Google ships an unverified OAuth client for now.
- **Token storage** stays on the existing on-disk-encrypted `AuthService` (`src/openhuman/credentials/`). Auth keys: `provider:<slug>` (canonical) with bare `<slug>` legacy fallback.
- **Auto-refresh-on-401** in `AuthedClient` keeps long-lived sessions alive across token expiry.
- **Direct provider clients** in `src/openhuman/providers/`: `gmail/`, `gcal/`, `gdrive/`, `github/`. They share a bearer helper, all go through `AuthedClient`, and slug → native dispatch lives in the same module.
- **Composio direct mode** routes Composio v3 calls against the user's personal tenant (their own Composio API key) instead of the deleted backend proxy. See "Composio" section below.

---

## LLM inference model

The fork has no hosted LLM backend. Three valid configurations:

1. **BYO cloud API key** (default). Configure via Settings → AI: add a `cloud_providers` row for OpenAI / Anthropic / OpenRouter / a custom OpenAI-compatible endpoint with your own key. Default route: OpenAI `gpt-5.4` + medium reasoning effort.
2. **Local Ollama** at `local_ai.base_url` (default `http://localhost:11434`). The factory's `ollama:<model>` provider string targets it.
3. **Local OpenAI-compatible runtime** (LM Studio, vLLM, llama.cpp's server, etc.). Add a `cloud_providers` row with `auth_style = None` pointing at the loopback URL.

**Workload routing** (`src/openhuman/inference/provider/factory.rs::provider_for_role`):

- `chat_provider`, `reasoning_provider`, `agentic_provider`, `coding_provider`, `memory_provider`, `embeddings_provider`, `heartbeat_provider`, `learning_provider`, `subconscious_provider` — each can set a `<slug>:<model>` string or fall back to `primary_cloud`.
- The legacy `"openhuman"` sentinel is dead. Any `cloud_providers` row with `auth_style = OpenhumanJwt` is migrated to `Bearer` (empty key) at config load — the user re-adds an API key under Settings → AI.

**Memory-tree chat** (`src/openhuman/memory/tree/chat/`): two production providers — `local::OllamaChatProvider` and `workload::WorkloadChatProvider` (delegates to the workload factory). The legacy `cloud::CloudChatProvider` was removed when the backend was deleted.

**TTS providers** (`src/openhuman/voice/factory.rs`):

| Provider | Module | Notes |
| --- | --- | --- |
| `cloud` | backend ElevenLabs proxy | requires backend session — effectively dead in this fork |
| `piper` | local Piper subprocess | broken on macOS upstream (dylib chain); kept as Linux/Windows option |
| `system` | macOS `say` + `afconvert` | macOS-only fallback while Piper is broken |
| `kokoro` | local OpenAI-compatible HTTP server | preferred path on Apple Silicon — mlx-audio with Kokoro, kokoro-fastapi, LM Studio audio mode |

Kokoro provider config: `kokoro_endpoint_url`, `kokoro_model`, `kokoro_voice` on `LocalAiConfig`. Setup recipe: [`kokoro-tts-provider-installation.md`](kokoro-tts-provider-installation.md).

---

## Composio

Composio is now the canonical integration broker for everything the deleted backend proxy used to cover (Gmail trigger events, GitHub trigger events, action execution against connected accounts). Two modes:

- **`composio.mode = "direct"`** (default in the fork): hits Composio v3 against the user's personal tenant (`backend.composio.dev/api/v3/*`) with their own API key. `composio_authorize`, `composio_execute`, `composio_sync`, `get_user_profile`, `delete_connection`, `refresh_all_identities`, trigger CRUD, and trigger reads (`list_triggers`, `list_available_triggers`) all route through this.
- **`composio.mode = "backend"`** (legacy): the upstream tinyhumansai backend proxy. Not usable in the closedhuman fork.

**Trigger delivery in direct mode** uses an embedded ngrok tunnel for the webhook URL (free static `<id>.ngrok-free.dev` domain). The receiver lives in `src/openhuman/composio/webhook_receiver/` and does Svix-style HMAC verification (`webhook-id` + `webhook-timestamp` + `webhook-signature` headers), parses the Composio v3 envelope, and publishes `DomainEvent::ComposioTriggerReceived` to the global event bus. Downstream pipeline (`trigger_triage` → `trigger_reactor` sub-agent) is unchanged.

**Trigger config schema parsing**: Composio v3 returns trigger `config` as JSON Schema (`{properties: {…}, required: ["owner", "repo"]}`). `composio::client::extract_required_keys` reads `required: string[]` first; the legacy per-field flat-map shape is kept as a fallback. `extract_default_values` walks `properties.<key>.default` so the renderer's `defaultConfig` payload is a flat `{key: value}` map, not the raw schema.

**Per-toolkit catalogue refresh**: `subagent_runner` and `agent::debug` now refresh the per-toolkit action list in direct mode via `composio::ops::fetch_direct_toolkit_actions` instead of using the (sometimes zero-action) cached snapshot. Fixes "Gmail isn't connected" appearing for users with an active Composio Gmail connection.

---

## Commands (from repo root)

```bash
pnpm dev                  # Vite dev server only (app workspace)
pnpm dev:app              # Full Tauri desktop dev (CEF runtime, loads env via scripts/load-dotenv.sh)
pnpm build                # Production UI build
pnpm typecheck            # tsc --noEmit (app workspace, aliased to `compile`)
pnpm compile              # Same as typecheck
pnpm lint                 # ESLint --cache
pnpm format               # Prettier write + cargo fmt
pnpm format:check         # Prettier check + cargo fmt --check

# Rust — core library + CLI
cargo check --manifest-path Cargo.toml
cargo build --manifest-path Cargo.toml --bin openhuman-core

# Rust — Tauri shell
cargo check --manifest-path app/src-tauri/Cargo.toml
pnpm rust:check           # Tauri shell check

# Native-OAuth validation
cargo run --bin oauth-connect -- google     # ends-to-end Google PKCE flow from CLI
cargo run --bin oauth-connect -- github     # GitHub equivalent
```

Note: `pnpm core:stage` is a no-op (echoes a message). The sidecar was removed in upstream PR #1061; core is linked in-process.

**Tests**: `pnpm test` (Vitest, app workspace) · `pnpm test:coverage` · `pnpm test:rust` (cargo test via `scripts/test-rust-with-mock.sh`).
**Quality**: ESLint + Prettier + Husky in `app`. Pre-push hook runs `pnpm rust:check` — pass `--no-verify` only for unrelated pre-existing breakage.

### Agent debug runners (`scripts/debug/`)

Bounded-output wrappers around the project test runners. Stdout stays summary-sized (so it fits in agent context); full output is teed to `target/debug-logs/<kind>-<suffix>-<timestamp>.log`. Add `--verbose` to also stream raw output. Prefer these over invoking Vitest / WDIO / cargo directly when iterating.

```bash
# Vitest
pnpm debug unit                                    # full suite
pnpm debug unit src/components/Foo.test.tsx        # one file (positional pattern)
pnpm debug unit -t "renders empty state"           # filter by test name
pnpm debug unit Foo -t "renders empty" --verbose

# WDIO E2E (one spec at a time)
pnpm debug e2e test/e2e/specs/smoke.spec.ts
pnpm debug e2e test/e2e/specs/cron-jobs-flow.spec.ts cron-jobs --verbose

# cargo tests (delegates to scripts/test-rust-with-mock.sh)
pnpm debug rust
pnpm debug rust json_rpc_e2e

# Inspect saved logs
pnpm debug logs                  # list 50 most recent
pnpm debug logs last             # print most recent (last 400 lines)
pnpm debug logs unit             # most recent matching prefix "unit"
pnpm debug logs last --tail 100
```

Files: `scripts/debug/{cli,unit,e2e,rust,logs,lib}.sh` plus `README.md`. Entry point is `pnpm debug` (`scripts/debug/cli.sh`).

### Coverage requirement (merge gate)

PRs must meet **≥ 80% coverage on changed lines**. Enforced by [`.github/workflows/coverage.yml`](.github/workflows/coverage.yml) using `diff-cover` over merged Vitest (`app/coverage/lcov.info`) and `cargo-llvm-cov` (core + Tauri shell) lcov outputs. Below the threshold the PR will not merge — add tests for new/changed lines, not just the happy path.

---

## Configuration

- **[`.env.example`](.env.example)** — Rust core, Tauri shell, backend URL (largely unused in this fork), logging, proxy, storage, AI binary overrides. Load via `source scripts/load-dotenv.sh`.
- **[`app/.env.example`](app/.env.example)** — `VITE_*` (core RPC URL, Sentry DSN, dev helpers). Copy to `app/.env.local`.
- **`~/.openhuman/config.toml`** — runtime config (`Config` struct in `src/openhuman/config/schema/types.rs`). Key sections:
  - `[local_ai]` — local-Ollama + Kokoro + per-workload provider routing (`chat_provider`, `reasoning_provider`, etc.).
  - `[[cloud_providers]]` — BYO API-key rows. `slug`, `endpoint`, `auth_style` (Bearer / Anthropic / None). API keys are NOT stored here; they live in `auth-profiles.json` via `AuthService` under `provider:<slug>`.
  - `[composio]` — `mode = "direct"` + `api_key_provider = "composio-direct"`.
  - `[memory_tree]` — extractor/summariser endpoints, embedder model.

**Frontend config** is centralized in [`app/src/utils/config.ts`](app/src/utils/config.ts). Read `VITE_*` there and re-export — **never** `import.meta.env` directly elsewhere.

**Rust config** uses a TOML `Config` struct (`src/openhuman/config/schema/types.rs`) with env overrides (`src/openhuman/config/schema/load.rs`). The `load.rs` migration sweeps `OpenhumanJwt` rows on every startup — see `migrate_openhuman_jwt_entries`.

---

## Testing

### Unit (Vitest)

- Co-locate as `*.test.ts` / `*.test.tsx` under `app/src/**`.
- Config: `app/test/vitest.config.ts`; setup: `app/src/test/setup.ts`.
- Run from repo root: `pnpm test` or `pnpm test:coverage`. (Inside `app/`, `pnpm test:unit` is also defined.)
- Prefer behavior over implementation. Use helpers in `app/src/test/`. No real network, no time flakes.

### Shared mock backend

Used by both unit and Rust tests. (The mock is still useful for testing the proxy-layer code that's been kept around — it's not exercised at runtime in the fork.)
- Core: `scripts/mock-api-core.mjs` · server: `scripts/mock-api-server.mjs` · E2E wrapper: `app/test/e2e/mock-server.ts`.
- Admin: `GET /__admin/health`, `POST /__admin/reset`, `POST /__admin/behavior`, `GET /__admin/requests`.
- Run manually: `pnpm mock:api`.

### E2E (WDIO — dual platform)

Full guide: [`gitbooks/developing/e2e-testing.md`](gitbooks/developing/e2e-testing.md).
- **Linux (CI)**: `tauri-driver` (WebDriver :4444).
- **macOS (local)**: Appium Mac2 (XCUITest :4723) on the `.app` bundle.
- Specs: `app/test/e2e/specs/*.spec.ts`. Helpers in `app/test/e2e/helpers/`. Config: `app/test/wdio.conf.ts`.

```bash
pnpm test:e2e:build
bash app/scripts/e2e-run-spec.sh test/e2e/specs/smoke.spec.ts smoke
pnpm test:e2e:all:flows
docker compose -f e2e/docker-compose.yml run --rm e2e   # Linux E2E on macOS
```

Use `element-helpers.ts` (`clickNativeButton`, `waitForWebView`, `clickToggle`) — never raw `XCUIElementType*`. Assert UI outcomes and mock effects.

### Deterministic core reset (E2E)

`app/scripts/e2e-run-spec.sh` creates and cleans a temp `OPENHUMAN_WORKSPACE` by default. `OPENHUMAN_WORKSPACE` redirects core config + storage away from `~/.openhuman`. Each spec gets a fresh in-process core inside the freshly-built Tauri bundle.

### Rust tests with mock

```bash
pnpm test:rust
bash scripts/test-rust-with-mock.sh --test json_rpc_e2e
```

---

## Frontend (`app/src/`)

**Provider chain** (`App.tsx`):
`Sentry.ErrorBoundary` → `Redux Provider` → `PersistGate` (with `PersistRehydrationScreen`) → `BootCheckGate` → `CoreStateProvider` → `SocketProvider` → `ChatRuntimeProvider` → `HashRouter` → `CommandProvider` → `ServiceBlockingGate` → `AppShell` (`AppRoutes` + `BottomTabBar` + walkthrough/mascot/snackbars).

`<AppUpdatePrompt />` is intentionally **not mounted** in the fork — the Tauri updater plugin is inactive. Re-mount when the fork has its own signed release feed.

No `UserProvider` / `AIProvider` / `SkillProvider` — auth and core snapshot live in `CoreStateProvider`, fetched via `fetchCoreAppSnapshot()` RPC (auth tokens are NOT in redux-persist; they live in the in-process core).

**State** (`store/`): Redux Toolkit slices — `accounts`, `channelConnections`, `chatRuntime`, `coreMode`, `deepLinkAuth`, `mascot`, `notification`, `providerSurface`, `socket`, `thread`. Persisted slices via redux-persist. Prefer Redux over ad-hoc `localStorage` (exception: ephemeral UI state like upsell dismiss flags).

**Services** (`services/`): singletons — `apiClient`, `socketService`, `coreRpcClient` + `coreCommandClient` (HTTP bridge to in-process core via Tauri IPC), `chatService`, `analytics`, `notificationService`, `webviewAccountService`, `daemonHealthService`, plus domain `api/*` clients.

**MCP** (`lib/mcp/`): JSON-RPC transport, validation, types over Socket.io.

**Routing** (`AppRoutes.tsx`, HashRouter): `/` (Welcome / DefaultRedirect), `/onboarding/*`, `/home`, `/human`, `/intelligence`, `/skills`, `/chat`, `/channels`, `/invites`, `/notifications`, `/rewards`, `/webhooks` (redirects to `/settings/webhooks-triggers`), `/settings/*` (including `/settings/triggers` for Composio direct trigger delivery, `/settings/voice` with the new Kokoro provider config, `/settings/ai` for BYO cloud providers + workload routing). Default catch-all is `DefaultRedirect`. There is no `/login`, no `/mnemonic` (recovery phrase moved to Settings), no `/agents`, no `/conversations`, no `/billing`.

**AI config**: bundled prompts in `src/openhuman/agent/prompts/` (also bundled via `app/src-tauri/tauri.conf.json` `resources`). Loaders in `app/src/lib/ai/` use `?raw` imports, optional remote fetch, and `ai_get_config` / `ai_refresh_config` in Tauri.

---

## Tauri shell (`app/src-tauri/`)

Thin desktop host. Top-level modules: `core_process`, `core_rpc`, `cdp`, `cef_preflight`, `cef_profile`, `dictation_hotkeys`, `file_logging`, `mascot_native_window`, `native_notifications`, `notification_settings`, `process_kill`, `process_recovery`, `screen_capture`, `window_state`, plus the per-provider scanner modules (`discord_scanner`, `gmessages_scanner`, `imessage_scanner`, `meet_scanner`, `slack_scanner`, `telegram_scanner`, `whatsapp_scanner`), `meet_audio` / `meet_call` / `meet_video`, `fake_camera`, `webview_accounts`, `webview_apis`.

**Core lifecycle**: `core_process::CoreProcessHandle` spawns the JSON-RPC server as an in-process tokio task and authenticates inbound RPC with a per-launch hex bearer (`OPENHUMAN_CORE_TOKEN`). On stale-listener detection (#1130) the handle revalidates the PID before force-killing so PID reuse can't kill an unrelated process. `restart_core_process` / `start_core_process` Tauri commands let the frontend cycle it for updates.

Registered IPC (see [`gitbooks/developing/architecture/tauri-shell.md`](gitbooks/developing/architecture/tauri-shell.md)) includes `greet`, `write_ai_config_file`, `ai_get_config`, `ai_refresh_config`, `core_rpc_relay`, `core_rpc_token`, `start_core_process`, `restart_core_process`, window commands, and `openhuman_*` daemon helpers. Always use `invoke('core_rpc_relay', ...)` for in-process RPC (avoids CORS preflight that `fetch()` would trigger).

The updater Tauri commands (`check_app_update`, `download_app_update`, `install_app_update`, `apply_app_update`) are still registered but error with "updater plugin not initialized" because `tauri.conf.json` has `updater.active: false`. They stay in the tree so the future re-enable is a config flip rather than a code restore.

### CEF child webviews — no new JS injection

Embedded provider webviews (`acct_*`, loading third-party origins like `web.telegram.org`, `linkedin.com`, `slack.com`, …) **must not** grow any new JavaScript injection. Do not add new `.js` files under `app/src-tauri/src/webview_accounts/`, do not append new blocks to `build_init_script` / `RUNTIME_JS`, and do not dispatch scripts via CDP `Page.addScriptToEvaluateOnNewDocument` / `Runtime.evaluate` for these webviews. The migrated providers (whatsapp, telegram, slack, discord, browserscan) load with **zero** injected JS under CEF by design — all scraping and observability runs natively via CDP in the per-provider scanner modules, and anything host-controlled that runs inside a third-party origin is a scraping/attack-surface liability.

New behavior for these webviews lives in:

- **CEF handlers** — `on_navigation`, `on_new_window`, `LoadHandler::OnLoadStart`, `CefRequestHandler::*` (wired in `webview_accounts/mod.rs`).
- **CDP from the scanner side** — `Network.*`, `Emulation.*`, `Input.*`, `Page.*` driven by the per-provider `*_scanner/` modules.
- **Rust-side notification/IPC hooks** — never cross into the renderer.

If a feature truly cannot be built this way (e.g. intercepting a click the page's JS preventDefaults), the correct answer is to **surface the limitation**, not to ship an init script. Legacy injection that already exists for non-migrated providers (`gmail`, `linkedin`, `google-meet` recipe files plus the `runtime.js` bridge) is grandfathered but should shrink, not grow.

Watch out for Tauri plugins that inject JS by default. `tauri-plugin-opener` ships `init-iife.js` (a global click listener that calls `plugin:opener|open_url` via HTTP-IPC) unless you build it with `.open_js_links_on_click(false)`. Any new plugin added to `app/src-tauri/src/lib.rs` must be audited for a `js_init_script` call — if found, opt out or configure around it.

---

## Rust core (`src/`)

- **`src/openhuman/`** — Domain logic. List of current domains changes frequently — discover with `ls src/openhuman/` rather than maintaining an enumeration here. RPC controllers in per-domain `rpc.rs` / `schemas.rs`; use `RpcOutcome<T>` per [`AGENTS.md`](AGENTS.md). Fork-specific newcomers worth knowing about:
  - **`openhuman/oauth/`** — PKCE primitives, loopback redirect server, Google + GitHub URL builders + token exchange + refresh, slug→native dispatch table. Entry point for the `oauth-connect` CLI.
  - **`openhuman/providers/`** — direct native API clients (Gmail / Calendar / Drive / GitHub) sharing a bearer helper + auto-refresh-on-401 wrapper.
  - **`openhuman/composio/webhook_receiver/`** — ngrok + Axum receiver, Svix-style HMAC verifier, Composio v3 envelope parser, subscription CRUD.
  - **`openhuman/inference/provider/factory.rs`** — workload-routed provider factory. `provider_for_role` resolves `<role>_provider` config → `<slug>:<model>`. `make_openhuman_backend` hard-errors with a Settings → AI pointer.
- **Skills + script execution**: `src/openhuman/skills/` is **metadata-only** (module header: "Skill metadata helpers and prompt-injection support") — it discovers, installs, and renders catalog entries but does **not** execute packages. The QuickJS / `rquickjs` runtime was removed in upstream PR #1061. New execution paths are being rebuilt under `src/openhuman/javascript/`, `src/openhuman/runtime_node/`, and `src/openhuman/runtime_python/` — check the current state of those modules before assuming a skill can run end-to-end. Skill registry source / bundler lives in **[github.com/tinyhumansai/openhuman-skills](https://github.com/tinyhumansai/openhuman-skills)** (not vendored in this tree); override with `VITE_SKILLS_GITHUB_REPO`.
- **Module layout rule**: new functionality goes in a **dedicated subdirectory** (`openhuman/<domain>/mod.rs` + siblings). **Do not** add new standalone `*.rs` files at `src/openhuman/` root (`dev_paths.rs` and `util.rs` are grandfathered, not a template).
- **Controller schema contract**: shared types in `src/core/types.rs` / `src/core/mod.rs` (`ControllerSchema`, `FieldSchema`, `TypeSchema`).
- **Domain schema files**: per-domain `schemas.rs` (e.g. `src/openhuman/cron/schemas.rs`), exported from domain `mod.rs`.
- **Controller-only exposure**: expose features to CLI and JSON-RPC via the controller registry. **Do not** add domain branches in `src/core/cli.rs` / `src/core/jsonrpc.rs`.
- **Light `mod.rs`**: keep domain `mod.rs` export-focused. Operational code in `ops.rs`, `store.rs`, `types.rs`, etc.
- **`src/core/`** — Transport only. Modules: `all`, `all_tests`, `auth`, `autocomplete_cli_adapter`, `cli`, `cli_tests`, `dispatch`, `event_bus/`, `jsonrpc`, `jsonrpc_tests`, `legacy_aliases`, `logging`, `memory_cli`, `observability`, `rpc_log`, `shutdown`, `socketio`, `types`, plus `agent_cli`. No heavy domain logic here. (There is no `src/core_server/` — older docs that reference `core_server` mean `src/core/`.)

### Modules deleted in the fork

These existed upstream and are explicitly **gone** in the closedhuman fork. Don't re-introduce them without alignment:

- `src/openhuman/billing/`, `src/openhuman/referral/`, `src/openhuman/team/` — all backend-proxy modules.
- `src/api/oauth_handoff*` — OAuth-via-backend machinery.
- `src/openhuman/memory/tree/chat/cloud.rs` — `CloudChatProvider` (replaced by `WorkloadChatProvider`).
- `BackendOAuthClient::validate_session_token` — unused.
- Login UI + login deep-link surfaces in `app/src/`.

### Controller migration checklist

- `src/openhuman/<domain>/mod.rs`: add `mod schemas;`, re-export `all_controller_schemas as all_<domain>_controller_schemas` and `all_registered_controllers as all_<domain>_registered_controllers`.
- `src/openhuman/<domain>/schemas.rs` defines `schemas`, `all_controller_schemas`, `all_registered_controllers`, and `handle_*` fns delegating to domain `rpc.rs`.
- Wire exports into `src/core/all.rs`. Remove migrated branches from `src/core/dispatch.rs`.

### Event bus (`src/core/event_bus/`)

Typed pub/sub + in-process typed request/response. Both singletons — use module-level functions; never construct `EventBus` / `NativeRegistry` directly.

- **Broadcast** (`publish_global` / `subscribe_global`) — fire-and-forget. Many subscribers, no return.
- **Native request/response** (`register_native_global` / `request_native_global`) — one-to-one typed dispatch keyed by method string. Zero serialization — trait objects, `mpsc::Sender`, `oneshot::Sender` pass through unchanged. Internal-only; JSON-RPC-facing work goes through `src/core/all.rs`.

Core types (all in `src/core/event_bus/`):

| Type | File | Purpose |
| --- | --- | --- |
| `DomainEvent` | `events.rs` | `#[non_exhaustive]` enum of all cross-module events |
| `EventBus` | `bus.rs` | Singleton over `tokio::sync::broadcast`; ctor is `pub(crate)` |
| `NativeRegistry` / `NativeRequestError` | `native_request.rs` | Typed request/response registry by method name |
| `EventHandler` | `subscriber.rs` | Async trait with optional `domains()` filter |
| `SubscriptionHandle` | `subscriber.rs` | RAII — drops cancel the subscriber |
| `TracingSubscriber` | `tracing.rs` | Built-in debug logger |

Singleton API: `init_global(capacity)`, `publish_global(event)`, `subscribe_global(handler)`, `register_native_global(method, handler)`, `request_native_global(method, req)`, `global()` / `native_registry()`.

Domains: `agent`, `memory`, `channel`, `cron`, `skill`, `tool`, `webhook`, `system`, `composio`.

Each domain owns a `bus.rs` with its `EventHandler` impls — e.g. `cron/bus.rs` (`CronDeliverySubscriber`), `webhooks/bus.rs` (`WebhookRequestSubscriber`), `channels/bus.rs` (`ChannelInboundSubscriber`), `composio/bus.rs` (`ComposioTriggerSubscriber`). Convention: `<Purpose>Subscriber` + `name()` returning `"<domain>::<purpose>"`.

**Adding events**: add variants to `DomainEvent`, extend the `domain()` match, create `<domain>/bus.rs`, register subscribers at startup, publish via `publish_global`.

**Adding a native handler**: define request/response types in the domain (owned fields, `Arc`s, channels — not borrows; `Send + 'static`, not `Serialize`). Register at startup keyed by `"<domain>.<verb>"`. Callers dispatch via `request_native_global`.

**Tests**: re-register the same method to override; or construct a fresh `NativeRegistry::new()` for isolation.

---

## Design

Premium, calm visual language — `primary-*` palette (ocean blue `#4A83DD` base), sage / amber / coral semantics, Inter + Cabinet Grotesk + JetBrains Mono, Tailwind with custom radii/spacing/shadows. Implementation tokens live in [`app/tailwind.config.js`](app/tailwind.config.js). **Always use the `primary-*` palette names**, not legacy `ocean-*` — the latter were renamed and don't compile.

## Shell vs app code

Tauri/Rust in the shell is a **delivery vehicle** (windowing, process lifecycle, IPC). Keep UI behavior and product logic in TypeScript/React (`app/`). Only grow Rust in the shell for hard platform/security reasons.

## Git workflow

The closedhuman fork is its own thing — no PRs back to `tinyhumansai/openhuman`. Day-to-day:

- **Never write code on `main`.** Branch off the latest `main` (`git fetch && git checkout -b <branch> origin/main`). All work happens on a feature branch; `main` stays clean and only advances via merged PRs against the fork's own `main`.
- **Check `git remote -v` first.** This checkout may have `origin` only, or `origin` + `upstream` (the original tinyhumansai repo, fetch-only). The fork's `origin` is what we push to.
- **Don't push to upstream.** If an `upstream` remote is present, treat it as fetch-only. Never `git push upstream`.
- **PRs target `main` on the fork.**
- Issue templates: [`.github/ISSUE_TEMPLATE/feature.md`](.github/ISSUE_TEMPLATE/feature.md), [`.github/ISSUE_TEMPLATE/bug.md`](.github/ISSUE_TEMPLATE/bug.md). PR template: [`.github/PULL_REQUEST_TEMPLATE.md`](.github/PULL_REQUEST_TEMPLATE.md). AI-authored text should follow them verbatim.
- **When the user asks you to push or open a PR, resolve blockers and push — don't prompt for permission.** If a pre-push hook fails on something unrelated to your changes (e.g. pre-existing fmt drift in code you didn't touch), push with `--no-verify` and call it out in the PR body. If the hook fails on your own changes, fix them and push again. Don't ask the user whether to bypass — just do the right thing and tell them what you did.
- **Pre-existing fmt drift** in `src/openhuman/memory/store/client.rs`, `src/openhuman/vault/{jobs,schemas,sync}.rs`, `src/openhuman/voice/factory.rs` keeps showing up after `cargo fmt`. Convention in the fork: drop those files from each commit (`git checkout --`) so commits stay scoped to the actual change.

---

## Coding philosophy

- **Unix-style modules**: small, sharp-responsibility units composed through clear boundaries.
- **Tests before the next layer**: ship unit tests for new/changed behavior before stacking features. Untested code is incomplete.
- **Docs with code**: new/changed behavior ships with matching rustdoc / code comments; update [`CHANGELOG.md`](CHANGELOG.md) when user-visible behavior changes, and `AGENTS.md` / architecture docs when rules or controller patterns change.

---

## Debug logging (must follow)

- Default to **verbose diagnostics** on new/changed flows so issues are easy to trace end-to-end.
- Log entry/exit, branches, external calls, retries/timeouts, state transitions, errors.
- Use stable grep-friendly prefixes (`[domain]`, `[rpc]`, `[ui-flow]`) and correlation fields (request IDs, method names, entity IDs). Fork-specific prefixes worth knowing: `[providers]`, `[chat-factory]`, `[composio-direct]`, `[voice-tts]`, `[voice-factory]`, `[oauth]`, `[config][cloud_providers]`.
- Rust: `log` / `tracing` at `debug` / `trace`. `app/`: namespaced `debug` + dev-only detail.
- **Never** log secrets or full PII — redact.
- Changes lacking diagnosis logging are incomplete.

---

## Feature design workflow

Specify → prove in Rust → prove over RPC → surface in the UI → test.

1. **Specify against the current codebase** — ground in existing domains, controller/registry patterns, JSON-RPC naming (`openhuman.<namespace>_<function>`). No parallel architectures.
2. **Implement in Rust** — domain logic under `src/openhuman/<domain>/`, schemas + handlers in the registry, unit tests until correct in isolation.
3. **JSON-RPC E2E** — extend [`tests/json_rpc_e2e.rs`](tests/json_rpc_e2e.rs) / [`scripts/test-rust-with-mock.sh`](scripts/test-rust-with-mock.sh) so RPC methods match what the UI will call.
4. **UI in Tauri app** — React screens/state using `core_rpc_relay` / `coreRpcClient`. Keep rules in the core.
5. **App unit tests** — Vitest.
6. **App E2E** — desktop specs for user-visible flows.

**Capability catalog**: when a change adds/removes/renames a user-facing feature, update `src/openhuman/about_app/` in the same work.

**Planning rule**: up front, define the **E2E scenarios (core RPC + app)** that cover the full intended scope — happy paths, failure modes, auth gates, regressions. Not testable end-to-end ⇒ incomplete spec or too-large cut.

---

## Key patterns

- **File size**: prefer ≤ ~500 lines; split growing modules.
- **Pre-merge** (code changes): Prettier, ESLint, `tsc --noEmit` in `app/`; `cargo fmt` + `cargo check` for changed Rust.
- **No dynamic imports** in production `app/src` code — static `import` / `import type` only. No `import()`, `React.lazy(() => import(...))`, `await import(...)`. For heavy optional paths, use a static import and guard the call site with `try/catch` or a runtime check. *Exceptions*: Vitest harness patterns in `*.test.ts` / `__tests__` / `test/setup.ts`; ambient `typeof import('…')` in `.d.ts`; config files (e.g. `tailwind.config.js` JSDoc).
- **Dual socket sync**: when changing the realtime protocol, keep `socketService` / MCP transport aligned with core socket behavior (see `gitbooks/developing/architecture.md` dual-socket section).
- **OpenAI-compatible everywhere**: the fork standardises on the OpenAI API shape for LLM (`/v1/chat/completions`) and audio (`/v1/audio/speech`) — any new provider should speak it, then plug into the existing factory rather than introducing a parallel transport.

---

## Platform notes

- **Vendored CEF-aware `tauri-cli`**: runtime is CEF; only the vendored CLI at `app/src-tauri/vendor/tauri-cef/crates/tauri-cli` bundles Chromium into `Contents/Frameworks/`. Stock `@tauri-apps/cli` produces a broken bundle (panic in `cef::library_loader::LibraryLoader::new`). `pnpm dev:app` and all `cargo tauri` scripts call `pnpm tauri:ensure` which runs [`scripts/ensure-tauri-cli.sh`](scripts/ensure-tauri-cli.sh). If overwritten, reinstall with `cargo install --locked --path app/src-tauri/vendor/tauri-cef/crates/tauri-cli`.
- **macOS deep links**: often require a built `.app` bundle, not just `tauri dev`.
- **Tauri environment guard**: use `isTauri()` (from `app/src/services/webviewAccountService.ts`) or wrap `invoke(...)` in `try/catch`; do not check `window.__TAURI__` directly — it is not present at module load and bypasses the established wrapper contract.
- **Core is in-process** (no sidecar): `core_rpc` reaches the embedded server at `http://127.0.0.1:<port>/rpc` with bearer auth via `OPENHUMAN_CORE_TOKEN`. `scripts/stage-core-sidecar.mjs` no longer exists; `pnpm core:stage` is a no-op echo. To run the core standalone for debugging, use `./target/debug/openhuman-core serve` (token at `{workspace}/core.token`, default `~/.openhuman-staging/core.token` under `OPENHUMAN_APP_ENV=staging`).
- **macOS code signing**: hardcoded Apple Signing Identity is no longer set in `pnpm dev:app`. Dev builds run unsigned; set `APPLE_SIGNING_IDENTITY` in your environment if you need a signed dev build.
- **ngrok for Composio trigger delivery**: the embedded webhook receiver needs an ngrok account + static `<id>.ngrok-free.dev` domain on the free plan. Paste authtoken + domain in Settings → Triggers; the receiver auto-starts at boot when both are present.
