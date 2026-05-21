# Run it today — local-OAuth build

A walkthrough for taking the `feat/local-oauth-no-backend` branch from a fresh
clone to a working desktop app. Single-user, local-first: no OpenHuman backend,
no app login, no Composio aggregator.

## TL;DR

```bash
# 1. Build everything (run from the repo root)
pnpm install
pnpm dev:app
```

`pnpm dev:app` chains `pnpm tauri:ensure` automatically — it installs
the vendored CEF-aware Tauri CLI on first run, then boots Vite + Tauri.

Once the Tauri window opens:

1. Go to **Settings → AI** → add your OpenAI key under the seeded
   `openai` provider entry.
2. *(Optional)* Run the loopback OAuth CLI for Google + GitHub if you
   want native Gmail/Calendar/Drive/GitHub tool execution.

You can do this entirely from the UI. The CLI flows below are the
scripted equivalents — useful for headless testing, but not required.

---

## What the build expects

| Component | How |
| --- | --- |
| **Default LLM** | OpenAI Responses API (`/v1/responses`) with `gpt-5.4` and `reasoning.effort = "medium"`. Set via `DEFAULT_MODEL = "openai:gpt-5.4"`. |
| **Auth storage** | Encrypted-on-disk `AuthService` (`<workspace>/auth-profiles.json`). |
| **OAuth providers** | Google + GitHub via the loopback flow (`127.0.0.1:<random>/oauth/callback`). Never touches a third party. |
| **Composio backend** | Gone. Native dispatch handles 9+ tool slugs (Gmail / Calendar / Drive / GitHub); unknown slugs hard-error with a pointer to `src/openhuman/oauth/native_dispatch.rs`. |
| **App login** | Removed. `/` redirects straight to `/home`. |

---

## Step 1 — Install dependencies

```bash
pnpm install
```

The Tauri shell needs the vendored CEF-aware CLI rather than stock
`@tauri-apps/cli` (the latter produces a bundle that panics inside
CEF's library loader). The `dev:app` and `tauri:build:ui` scripts in
`app/package.json` automatically chain `pnpm tauri:ensure`, which
shells out to `scripts/ensure-tauri-cli.sh` and installs
`app/src-tauri/vendor/tauri-cef/crates/tauri-cli` into `~/.cargo/bin`
the first time it runs.

If you want to run the ensure step explicitly (e.g. before a CI build
or after blowing away your cargo bin dir):

```bash
pnpm --filter openhuman-app tauri:ensure
```

It's idempotent — subsequent calls are a fast no-op once the vendored
CLI is installed.

## Step 2 — Run the desktop app

```bash
pnpm dev:app
```

This builds the Rust core in-process, spins up the Tauri shell, and
opens the desktop window. There is no more sidecar `openhuman-core`
process — the JSON-RPC server is a tokio task inside the GUI process
(see `app/src-tauri/src/core_process.rs`).

You should land directly on `/home`. If you see a blank screen and a
spinning loader, that means `CoreStateProvider` is still bootstrapping;
give it a few seconds.

## Step 3 — Configure OpenAI from the UI

1. Open **Settings → AI**.
2. The migration has already seeded a cloud provider with slug
   `openai`, endpoint `https://api.openai.com/v1`, `default_model =
   "gpt-5.4"`.
3. Toggle the OpenAI provider on (or click its API-key chip) and paste
   your `sk-...` key.
4. The chat panel should now respond against `gpt-5.4` via the
   Responses API. The `reasoning.effort = "medium"` field is added
   automatically for any `gpt-5*` / `o1*` / `o3*` / `o4*` model
   (`ResponsesReasoning::default_for` in
   `src/openhuman/inference/provider/compatible_types.rs`).

### CLI equivalent

If you'd rather store the key without launching the UI:

```bash
cargo build --bin openhuman-core
./target/debug/openhuman-core rpc auth_store_provider_credentials \
  --params '{"provider":"openai","token":"sk-..."}'
```

The key lands in the same encrypted `auth-profiles.json` the UI uses.

## Step 4 — *(Optional)* Connect Google + GitHub natively

You only need this if you want the agent to call Gmail / Calendar /
Drive / GitHub tools. Without it, the LLM still works fine for plain
chat.

Native OAuth requires a build-time client ID per provider (the user is
on a private/personal fork, so unverified Google clients are
acceptable). Build the connect binary with the IDs baked in:

```bash
OPENHUMAN_GOOGLE_OAUTH_CLIENT_ID=<your-google-oauth-client-id> \
OPENHUMAN_GITHUB_OAUTH_CLIENT_ID=<your-github-oauth-client-id> \
  cargo build --bin oauth-connect
```

Then run it once per provider:

```bash
./target/debug/oauth-connect --provider google
./target/debug/oauth-connect --provider github
```

Each invocation:

1. Spins up a one-shot HTTP server on a random `127.0.0.1` port.
2. Opens the provider's consent URL in your system browser.
3. Captures the redirect, exchanges the code via PKCE, and persists the
   resulting access + refresh tokens to `auth-profiles.json` under
   `google` / `github`.
4. Exits.

Tokens auto-refresh on HTTP 401 via `bearer::AuthedClient` — no manual
re-auth until the provider revokes the refresh token (Google unverified
apps: ~7 days; GitHub: indefinite for classic OAuth, otherwise per the
expiring-OAuth-App policy).

The 9 native tool slugs available without ever touching a third party:

- **Gmail**: `GMAIL_SEND_EMAIL`, `GMAIL_FETCH_EMAILS`,
  `GMAIL_DELETE_EMAIL`, `GMAIL_ADD_LABEL_TO_EMAIL`
- **Calendar**: `GOOGLECALENDAR_EVENTS_LIST` /
  `GOOGLECALENDAR_FIND_EVENT`, `GOOGLECALENDAR_EVENTS_GET`,
  `GOOGLECALENDAR_CREATE_EVENT`
- **Drive**: `GOOGLEDRIVE_LIST_FILES` / `GOOGLEDRIVE_FIND_FILE`,
  `GOOGLEDRIVE_GET_FILE_METADATA`, `GOOGLEDRIVE_CREATE_FILE` /
  `GOOGLEDRIVE_CREATE_FILE_FROM_TEXT`
- **GitHub**: `GITHUB_USERS_GET_AUTHENTICATED`,
  `GITHUB_CREATE_AN_ISSUE`,
  `GITHUB_LIST_REPOSITORIES_FOR_THE_AUTHENTICATED_USER`

Adding more slugs is a single-arm change in
`src/openhuman/oauth/native_dispatch.rs` plus a typed function in
`src/openhuman/providers_native/`.

## Step 5 — Smoke-test

From the Tauri window:

- Open the chat panel and send "hi" — confirm the response comes back.
- *(If you did Step 4)* ask the agent to "list my next 3 calendar
  events" or "create a GitHub issue in `<owner>/<repo>` titled foo" —
  confirm it executes via native dispatch (logs prefixed
  `[bearer]` / `[oauth]` in `target/debug-logs/`).
- Try an unwired slug (e.g. `NOTION_SEARCH`) — confirm the agent
  surfaces the `"no native dispatcher"` error verbatim rather than
  silently hitting any backend.

---

## Troubleshooting

### "no cloud provider configured for slug 'openai'"

The migration didn't run, or you launched against a pre-existing
`config.toml` from before the refactor. Easiest fix: delete the file
and let the migration re-seed:

```bash
rm ~/.openhuman/config.toml   # or wherever your workspace lives
```

Then restart the Tauri app.

### "encryption key on this device no longer matches"

A prior login dropped encrypted state that the new build can't read.
Use **Settings → Advanced → Clear app data** (or remove the workspace
dir manually) and restart.

### Tauri panic in `cef::library_loader::LibraryLoader::new`

The stock `@tauri-apps/cli` ran instead of the vendored one. Re-run
`pnpm --filter openhuman-app tauri:ensure` and rebuild.

### gpt-5.4 returns 404 / "model not found"

OpenAI hasn't enabled that model ID on your account yet, or the name
drifted. Swap `DEFAULT_MODEL` in
`src/openhuman/config/schema/types.rs:30` to whichever model your key
has access to (e.g. `"openai:gpt-5"`, `"openai:gpt-4.1"`), rebuild, and
restart. The reasoning-effort field auto-skips for non-reasoning
families.

---

## What's NOT working yet

Frontend pages that were tightly coupled to backend-only domains
(rewards, invites, billing, team, Composio toolkit catalog) still
render in the app but their backing RPCs error out. They're harmless
— just don't expect rewards or billing to do anything. Phase 6 of
`tasks/todo.md` covers replacing or deleting each.
