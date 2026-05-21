# closedhuman

**A fork of [OpenHuman](https://github.com/tinyhumansai/openhuman) that actually runs locally.**

The upstream project markets itself as "Private. Simple. Extremely powerful." and frames the desktop app as a local-first personal AI. In practice every meaningful workload — chat, memory ingestion, voice synthesis, integration auth, even feature gates and onboarding redirects — went through a hosted OpenHuman backend. Take the backend away (or just let your session expire) and the app collapsed into `SESSION_EXPIRED` retry loops, broken settings tiles, and a chat input that refused to render because a JWT it didn't actually need was missing.

This fork rips that backend out, fixes the deep bugs that came with it, and rebuilds the connector layer on top of native OAuth (Google + GitHub) plus Composio direct-mode against your own API key. You bring your own LLM key (OpenAI, Anthropic, or any OpenAI-compatible endpoint) or run inference locally (Ollama, LM Studio, mlx-audio with Kokoro for TTS). No accounts to create. No subscription. No telemetry to a third party's backend. The whole thing dies with the GUI on Cmd+Q.

It is opinionated, not polished. If you want a hand-held experience, use the upstream build. If you want a personal AI that runs entirely on your machine and connectors you control, this is the fork.

---

## Why this fork exists

Upstream's repo description: _"Personal AI super intelligence. Private, Simple and extremely powerful."_

The locality promise reads cleanly. The implementation didn't honour it.

### What the OpenHuman backend actually was

Upstream had a hosted product backend at `api.openhuman.ai` that the desktop app could not function without. Concretely, it was doing:

- **LLM proxy.** Every chat call routed `POST /openai/v1/chat/completions` against the backend, which then talked to OpenAI / Anthropic on its own keys and billed against an internal "tier" system. The "your own API key" surface didn't actually carry the key into production — it got auto-migrated to the backend session JWT.
- **OAuth handoff for integrations.** Gmail, Calendar, Drive, GitHub, Slack — every connector OAuth flow bounced through the backend, which held the tokens and acted as an aggregator (via Composio). Your provider tokens never lived on your machine.
- **TTS proxy.** ElevenLabs synthesis routed through the backend's `/openai/v1/audio/speech` with backend-side billing.
- **Voice transcription proxy.** Whisper called through the backend.
- **Telemetry, billing, referral, team management.** Hosted only — no local equivalent.
- **Feature gates.** UI tiles like the bottom tab bar refused to render until a backend session JWT was present, even on a fresh install where one couldn't possibly exist yet.

When you bought into "private and local," you got a desktop wrapper around a SaaS that happened to render in Chromium.

### The forced-plan implementation

The product was clearly built around the assumption that a hosted tier system was the monetisation. That's a defensible business model — but it bled into the architecture in ways that made the "private" framing misleading:

- The free tier had hard rate limits on LLM calls counted server-side.
- "Bring your own OpenAI key" was a UI knob in `cloud_providers` that, on save, got silently migrated to `auth_style = OpenhumanJwt`. Your key wasn't ignored by accident — it was systematically swapped for the backend session at config load (`migrate_legacy_fields` flipped `Bearer` → `OpenhumanJwt` for legacy `type = "openhuman"` rows, which any pre-existing row had).
- The "local AI" toggle gated whether the runtime _probe_ fired, but the chat path still preferred the backend when both were available.
- The Settings → Connections panel had a "Google" tile marked `comingSoon: true` that did nothing — meanwhile the real Gmail OAuth had to be done through Composio's web dashboard, which the docs didn't mention.
- The orchestrator's system prompt hardcoded the line `head to Settings → Connections → Gmail to hook it up` — pointing users at the stub tile when the actual working path was elsewhere.

### Bugs the rewrite uncovered

Once the backend was removed, the rot underneath became visible. Each of these was a real, reproducible failure mode on a clean install of the upstream build:

| Bug                                                                  | Why it broke                                                                                                                                                                                                                                                                                                  | Fix                                                                                                                                                   |
| -------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| `SESSION_EXPIRED: backend session not active` on every chat call     | `provider/ops.rs::create_backend_inference_provider` silently constructed `OpenHumanBackendProvider` whenever `inference_url` was missing — regardless of whether the user had configured a `cloud_providers` row with their own key.                                                                         | Hard-error with a Settings → AI pointer when no inference URL is configured.                                                                          |
| User-supplied OpenAI keys silently swapped for backend JWT           | `cloud_providers.rs::migrate_legacy_fields` rewrote `auth_style = Bearer` → `OpenhumanJwt` for any row with legacy `type = "openhuman"`.                                                                                                                                                                      | Migration removed; one-time sweep converts existing `OpenhumanJwt` rows back to `Bearer` (empty key) and asks the user to re-add.                     |
| Memory-tree ingestion broke without a backend session                | `memory/tree/chat/cloud.rs::CloudChatProvider` hardcoded the OpenHuman backend regardless of workload routing config.                                                                                                                                                                                         | File deleted; replaced by `WorkloadChatProvider` which honours `memory_provider` config.                                                              |
| User-chosen model IDs silently rewritten                             | `local_ai` config layer normalised the user's model id into upstream's tier names.                                                                                                                                                                                                                            | Identity-preserving — what you type is what's sent.                                                                                                   |
| Dead defaults re-injected into `config.toml` on every save           | A migration pass kept rewriting deleted backend URLs into config every time the file was touched.                                                                                                                                                                                                             | Default injection removed.                                                                                                                            |
| Bottom tab bar invisible on fresh install                            | Component gated on `sessionToken` being non-null. There is no session on a fresh install.                                                                                                                                                                                                                     | Gate removed.                                                                                                                                         |
| Chat input blocked behind missing JWT                                | The socket only connected when a session token was present; chat input gated on socket-connected state.                                                                                                                                                                                                       | Socket connects unconditionally; chat input no longer reads the session field.                                                                        |
| `/` route bypassed onboarding                                        | Welcome routing landed users directly on `/home` instead of the onboarding pipeline.                                                                                                                                                                                                                          | `/` now goes through `DefaultRedirect`.                                                                                                               |
| Subconscious workload ignored `subconscious_provider` config         | Engine called `build_chat_provider(_, ChatConsumer::Summarise)` which hardcoded the `memory` workload, ignoring the user's `subconscious_provider` selection.                                                                                                                                                 | New `build_chat_provider_for_role(_, "subconscious", _)`.                                                                                             |
| Composio direct mode silently dropped trigger events                 | The bus subscriber had a `if config.composio.mode == "direct" { return; }` guard, killing every webhook delivery even when the mode was explicitly enabled.                                                                                                                                                   | Gate removed.                                                                                                                                         |
| Composio v3 trigger config parsed wrong                              | The direct-mode parser expected a flat `{field: {required: true}}` map; Composio v3 actually returns a JSON Schema (`{properties, required: ["owner", "repo"]}`). Result: GitHub trigger UI showed plain toggles with no way to enter `owner`/`repo`.                                                         | Parser reads JSON Schema first, flat-map as legacy fallback.                                                                                          |
| Sub-agent saw zero Gmail tools despite an active Composio connection | The integrations sub-agent reused a session-start catalogue snapshot that could legitimately contain zero actions for a toolkit whose OAuth had completed after the snapshot. Resulting agent reply: "Gmail isn't connected here."                                                                            | New `fetch_direct_toolkit_actions` refreshes per-toolkit catalogue at spawn time.                                                                     |
| Kokoro TTS POSTed the mascot's ElevenLabs voice id                   | The TTS factory's voice-override gate dropped Piper-shaped ids (correct) but let ElevenLabs CamelCase ids (`Rachel`) through, where they hit the local Kokoro server as unknown voices.                                                                                                                       | Positive-match against Kokoro's published naming convention (`^[abefjz][mf]_…`).                                                                      |
| Kokoro response read errored opaquely on chunked streams             | Body collector used `reqwest::Response::bytes()`, which surfaces a single unstructured "error decoding response body" when the transfer-encoded stream ends abnormally. No bytes received, no content-type, no way to tell server-crashed-mid-synth from transfer-encoding mismatch.                          | Streaming accumulator + RIFF/WAVE magic sniff.                                                                                                        |
| Piper TTS broken on macOS upstream                                   | The only published macOS release (`2023.11.14-2`) ships a binary that depends on `@rpath/libespeak-ng.1.dylib`, `@rpath/libpiper_phonemize.1.dylib`, `@rpath/libonnxruntime.1.14.1.dylib` — none of which are bundled in the tarball, and the binary has no `LC_RPATH` entries to find them. No upstream fix. | Replaced with a Kokoro provider that talks OpenAI Audio API to a local server (mlx-audio on Apple Silicon — MLX-accelerated, ~real-time on M-series). |
| Auto-updater pointed at upstream releases                            | `tauri.conf.json` had `updater.endpoints = ["https://github.com/tinyhumansai/openhuman/releases/latest/download/latest.json"]`. If installed in the fork, it would have downloaded and applied upstream binaries on top of the fork.                                                                          | Disabled at three layers (Tauri plugin config, `<AppUpdatePrompt />` mount removed, Settings → About card removed).                                   |
| Hardcoded `APPLE_SIGNING_IDENTITY` in `pnpm dev:app`                 | Dev script set someone else's signing identity, breaking `dev:app` on any machine that wasn't upstream's CI.                                                                                                                                                                                                  | Removed.                                                                                                                                              |
| Telegram / Discord / iMessage channel listeners didn't hot-reload    | Pairing a new account required restarting the app for the listeners to pick it up.                                                                                                                                                                                                                            | Hot-reload on connect / disconnect.                                                                                                                   |
| `useUsageState` spammed deleted RPCs                                 | The hook polled `team/*` and `billing/*` RPCs that no longer exist in the fork (and never existed for non-paying users in upstream).                                                                                                                                                                          | Hook gutted.                                                                                                                                          |
| "Reconnecting to backend" overlay covered Home                       | Connectivity overlay was triggered by a stale boot-check that always reported "disconnected" without a backend.                                                                                                                                                                                               | Overlay removed.                                                                                                                                      |
| `list_models` failed against loopback URLs                           | Runtime HTTP client applied the system proxy even when the target was `127.0.0.1`.                                                                                                                                                                                                                            | Loopback bypass + URL/source surfaced in errors.                                                                                                      |
| Channel + threads providers bypassed workload routing                | Two more code paths constructed providers directly instead of routing through the factory, so the user's per-workload config was ignored.                                                                                                                                                                     | Routed through `provider_for_role`.                                                                                                                   |
| Composio op surface fragmented across modes                          | `composio_sync`, `get_user_profile`, `delete_connection`, `refresh_all_identities` only worked through the backend path; the direct-mode arm was a stub.                                                                                                                                                      | All re-routed through the mode-aware factory.                                                                                                         |
| Vault sync was synchronous + blocking                                | A single sync ran on the request thread; the UI froze for the duration.                                                                                                                                                                                                                                       | Async job worker with progress callbacks + matching UI.                                                                                               |
| Settings → Connections "Google" tile marked `comingSoon: true`       | Tile was a stub; the real Google OAuth path was Composio (undocumented).                                                                                                                                                                                                                                      | Tile stays cosmetic until we ship a direct OAuth flow that bypasses Composio for Google. The Composio dialog is the working path.                     |

Full per-commit log of the rewrite is in [`CHANGELOG.md`](CHANGELOG.md).

---

## What this fork is, in one paragraph

A single-user desktop AI app that runs on your machine, against your own provider accounts and your own LLM key (or local Ollama / LM Studio). All OAuth tokens live encrypted on disk in the existing `AuthService`, not on someone else's server. Composio direct-mode handles the long tail of SaaS connectors (Gmail / Calendar / GitHub / Slack / Notion / …) against your personal Composio tenant. TTS goes to a local OpenAI-compatible server (Kokoro on mlx-audio is the recommended path on Apple Silicon). There is no app login, no `/billing` page, no team management, no telemetry route to a backend you don't own. The auto-updater is off — you build the binary yourself, or pull releases from your own fork if you set up that pipeline.

---

## Installation

You will need:

- **Rust** (`rustup`, stable toolchain)
- **Node + pnpm** (the repo enforces pnpm via `packageManager`)
- **Python 3.12** if you want Kokoro TTS on macOS (managed via `uv`, see below)
- A **Google Cloud project** for Gmail / Calendar / Drive OAuth
- A **GitHub OAuth app** for the GitHub connector
- A **Composio account** (free tier is fine) for the broader connector set
- An **ngrok account** (free tier — gives one static `<id>.ngrok-free.dev` domain) for receiving Composio trigger events
- One of:
  - An **OpenAI API key** (or Anthropic, OpenRouter, any OpenAI-compatible)
  - **Ollama** installed locally
  - **LM Studio** running locally

The build is straightforward; the OAuth and connector setup is what takes most of the time.

### 1. Clone and build

```bash
git clone <your-fork-url> closedhuman
cd closedhuman

pnpm install
pnpm tauri:ensure   # installs the vendored CEF-aware tauri-cli; required on first run

# Dev mode (recommended while you set things up):
pnpm dev:app

# Production binary:
pnpm tauri build
```

The vendored `tauri-cli` is non-negotiable on macOS — the stock `@tauri-apps/cli` builds a bundle that panics in `cef::library_loader::LibraryLoader::new` because it doesn't bundle Chromium into `Contents/Frameworks/`. `pnpm tauri:ensure` is idempotent; re-run it if your toolchain ever drifts.

`pnpm dev:app` loads env via `scripts/load-dotenv.sh`. Copy `.env.example` → `.env` and `app/.env.example` → `app/.env.local` if you have local overrides. Defaults work for everything except the OAuth client IDs (next section).

### 2. Google OAuth setup

The fork uses native OAuth 2.0 PKCE with a loopback redirect (RFC 8252 §7.3). Tokens never leave your machine. You need your own Google Cloud OAuth client because nobody distributes a shared one with secrets baked in.

**At [console.cloud.google.com](https://console.cloud.google.com):**

1. **Create a project** (or use an existing one).
2. **Enable APIs** — go to _APIs & Services → Library_ and enable:
   - Gmail API
   - Google Calendar API
   - Google Drive API
3. **Configure the OAuth consent screen** — _APIs & Services → OAuth consent screen_:
   - User type: **External** (if you're a personal Google account) or **Internal** (Workspace).
   - Scopes: add the read/write scopes for the APIs you enabled. The fork requests them per-connector at runtime.
   - **Add yourself as a test user** while the app is unverified. Unverified apps can have up to 100 test users.
4. **Create the OAuth client** — _APIs & Services → Credentials → Create Credentials → OAuth client ID_:
   - Application type: **Desktop app**.
   - Note the **client ID** and **client secret** (Google's "desktop app" clients have one even though native PKCE technically doesn't need it — Google's token endpoint still validates it).
5. **Build the binary with your client ID + secret baked in.** Currently these are compile-time constants in `src/openhuman/oauth/`. Open `src/openhuman/oauth/google.rs` (or wherever the constants live in the build you cloned — they get moved around) and paste in your `client_id` and `client_secret`. Then rebuild with `pnpm tauri build`.
   - _(Future-proofing note: moving these to env / config so a fresh user doesn't need to rebuild is on the todo list. For now it's a build-time secret.)_

**Validate end-to-end without launching the GUI:**

```bash
cargo run --bin oauth-connect -- google
```

The CLI spawns the loopback server, opens your browser, completes the PKCE handshake, persists the tokens via `AuthService`, and prints the resulting profile. If this works, the GUI's "Connect Google" flow will work too.

### 3. GitHub OAuth setup

Native PKCE works against GitHub without a client secret.

**At [github.com/settings/developers](https://github.com/settings/developers):**

1. _OAuth Apps → New OAuth app_.
2. **Application name**: anything (`closedhuman` is fine).
3. **Homepage URL**: anything.
4. **Authorization callback URL**: `http://127.0.0.1/callback` (the exact port is allocated at runtime — GitHub lets you register `127.0.0.1` and matches any port).
5. Note the **client ID** (no secret needed).
6. Paste the client ID in `src/openhuman/oauth/github.rs` (same pattern as Google) and rebuild.

Smoke test:

```bash
cargo run --bin oauth-connect -- github
```

### 4. Composio setup (for everything else)

Native OAuth covers Google + GitHub. Everything beyond that (Slack, Notion, Discord, YouTube, Calendly, Linear, Jira, Figma, HubSpot, Salesforce, …) goes through **Composio direct mode**. Composio handles the OAuth dance against the third-party service, hands the resulting token to your local app, and exposes a uniform action catalogue.

**At [app.composio.dev](https://app.composio.dev):**

1. **Sign up** for an account (free tier is fine for personal use).
2. _Settings → API keys → Create new key_. Copy the API key.
3. In the app: _Settings → Composio → API key_. Paste it.
4. Restart the app (`Cmd+Q` then `pnpm dev:app`) so the new key is picked up.

**Connect a toolkit:**

1. In the app, open the Channels / Integrations grid.
2. Click the toolkit you want (Slack, Notion, etc.).
3. In the Manage dialog, scroll to the toolkit's section and click **Connect**.
4. Composio's OAuth flow opens in your default browser. Authorise the third-party scopes.
5. Once the dot turns green ("connected"), you're done — the agent can now call that toolkit's actions.

**Enable triggers** (so the agent reacts to incoming events, not just outgoing tool calls):

1. In the same Manage dialog, scroll to the **Triggers** section.
2. Toggle the triggers you want (e.g. `GMAIL_NEW_GMAIL_MESSAGE`, `GITHUB_COMMIT_EVENT`).
3. For static triggers that need config (GitHub triggers want `owner` + `repo`), the inline form expands automatically when you toggle on — fill it in and click Enable.

Triggers won't deliver events to your local app until you also set up ngrok (next section).

### 5. ngrok setup (for receiving Composio trigger events)

Composio delivers trigger events as outbound webhooks. For them to reach your local app, Composio's servers need a public URL that resolves back to your machine. ngrok provides exactly that with a free static domain.

**Why ngrok specifically:**

- ngrok's free tier gives you **one persistent static `<id>.ngrok-free.dev` domain** that doesn't change between sessions. Cloudflare Tunnel's free tier requires a user-owned domain managed through their nameservers, which is a heavier setup. Other tunnels either rotate URLs or aren't free.
- We embed the ngrok agent SDK (`ngrok` crate, `0.14+`) directly in the Rust core. No separate `ngrok` process to manage — the receiver starts and stops with the app.
- The receiver does Svix-style HMAC verification on every incoming webhook (Composio signs each delivery with a per-subscription secret), so even if someone discovers your ngrok URL they can't forge events.

**Setup:**

1. **Sign up** at [ngrok.com](https://ngrok.com) (free, no credit card).
2. **Copy your authtoken** from [dashboard.ngrok.com/get-started/your-authtoken](https://dashboard.ngrok.com/get-started/your-authtoken). It's a long hex string.
3. **Reserve your static domain** at [dashboard.ngrok.com/domains](https://dashboard.ngrok.com/domains). If one isn't shown, click "New domain" — the free plan auto-provisions one (`<random-id>.ngrok-free.dev`).
4. In the app: _Settings → Triggers_:
   - **ngrok authtoken** — paste it.
   - **ngrok static domain** — paste it (without the `https://` prefix, just `<id>.ngrok-free.dev`).
   - Click **Test tunnel**. The green checkmark means the receiver is up.
5. The first time you enable a trigger on any toolkit, the app calls `POST /api/v3/webhook_subscriptions` against Composio with your ngrok URL — one subscription covers every trigger type you'll ever enable, so this is a one-shot setup.

If the Test button fails: check that the authtoken matches the domain (they're tied to the same ngrok account), and that nothing else on your machine is bound to port 8765 (configurable under "Advanced" in the same panel).

### 6. macOS TTS — Kokoro via mlx-audio

The macOS-native `say(1)` fallback works out of the box but sounds like 2010. For real TTS on Apple Silicon, run Kokoro on mlx-audio — MLX-accelerated, ~real-time, decent quality.

This is a multi-step install because mlx-audio's `[tts]` extras conflict with Python 3.14 (which `uv` picks by default) and the Kokoro pipeline downloads a spaCy model at runtime via a subprocess that needs `pip` available in the tool's venv. Detailed reminder in [`kokoro-tts-provider-installation.md`](kokoro-tts-provider-installation.md). The one-shot install command:

```bash
uv tool install --force --python 3.12 \
  'mlx-audio[tts] @ git+https://github.com/Blaizzy/mlx-audio.git' \
  --prerelease=allow \
  --with 'spacy>=3.8,<4' \
  --with 'fastapi>=0.95.0' \
  --with 'uvicorn[standard]>=0.22.0' \
  --with 'python-multipart>=0.0.22' \
  --with 'webrtcvad>=2.0.10' \
  --with 'setuptools<81' \
  --with 'misaki[en]'
```

Then pre-install the spaCy English model (one line, no shell line-breaks, otherwise the URL gets mangled):

```bash
URL='https://github.com/explosion/spacy-models/releases/download/en_core_web_sm-3.8.0/en_core_web_sm-3.8.0-py3-none-any.whl'
uv pip install --python ~/.local/share/uv/tools/mlx-audio/bin/python "$URL"
```

Run the server:

```bash
mlx_audio.server --host 127.0.0.1 --port 8880 --realtime-model mlx-community/Kokoro-82M-bf16
```

First boot downloads the Kokoro weights (~400 MB) into `~/.cache/huggingface/`; subsequent boots are instant.

Verify the server works before touching the app:

```bash
curl -sS http://127.0.0.1:8880/v1/audio/speech \
  -H 'Content-Type: application/json' \
  -d '{"model":"kokoro","input":"hello from kokoro","voice":"af_bella","response_format":"wav"}' \
  --output /tmp/k.wav && afplay /tmp/k.wav
```

In the app: _Settings → Voice_:

| Field                   | Value                                       |
| ----------------------- | ------------------------------------------- |
| Text-to-Speech Provider | **Kokoro (local OpenAI-compatible server)** |
| Endpoint                | `http://localhost:8880`                     |
| Model                   | `mlx-community/Kokoro-82M-bf16`             |
| Default voice           | `af_bella`                                  |

Save by tabbing out of each field (`onBlur` persists). The mascot's spoken-reply path routes through the same factory dispatch, so picking Kokoro here switches every TTS surface in the app — no separate mascot wiring needed.

Other Kokoro voices: `af_heart` (American female, alternate), `am_michael` (American male), `bf_emma` (British female), `bm_lewis` (British male), plus `jf_*` (Japanese), `zf_*` (Chinese), `ef_*` (Spanish), `ff_*` (French). Non-English voices need the matching `misaki[<lang>]` extra in the install above.

### 7. LLM provider — bring your own key (or run locally)

The fork has no hosted LLM backend. Three valid configurations, pick whichever fits:

**Option A — BYO cloud API key** (easiest, default). At _Settings → AI_:

1. Click "Add provider".
2. Choose **OpenAI**, **Anthropic**, **OpenRouter**, or **Custom** (any OpenAI-compatible endpoint — Groq, Together, Fireworks, …).
3. Paste your API key into the masked field.
4. Optionally pick a default model per workload (chat / reasoning / agentic / coding / memory / embeddings / subconscious). Each gets its own provider config — you can put chat on Anthropic Claude and embeddings on a local model if you want.

The default config routes everything to OpenAI `gpt-5.4` with medium reasoning effort. Override at any workload row.

**Option B — Local Ollama.** Install [Ollama](https://ollama.com), pull a model (`ollama pull llama3.1:70b`), and at _Settings → AI_:

1. The app auto-detects Ollama at `http://localhost:11434`.
2. In the workload routing section, set the workload to `ollama:<model>` (e.g. `ollama:llama3.1:70b`).

**Option C — LM Studio or any OpenAI-compatible local server.** Spin up the server, then at _Settings → AI_ add a Custom provider with `auth_style = None`, endpoint `http://localhost:<port>/v1`, and use whatever model id the server exposes.

You can mix all three — chat on cloud OpenAI, memory ingestion on local Ollama, voice/embeddings on local models. Each workload reads its own `*_provider` config.

---

## After installation

- **Boot it**: `pnpm dev:app` for dev, or the built `.app` / binary for production.
- **First run**: walk through onboarding. The fork doesn't ask you to log in — onboarding is purely "set your provider keys, connect your toolkits."
- **When something breaks**: tail `~/.openhuman/logs/openhuman.$(date +%Y-%m-%d).log` and grep for the relevant prefix (`[providers]`, `[chat-factory]`, `[composio-direct]`, `[voice-tts]`, `[voice-factory]`, `[oauth]`).
- **When you want to update**: pull the fork, `pnpm install`, rebuild. The auto-updater is off — there's no upstream release feed for the fork yet, and you don't want it pulling builds from `tinyhumansai/openhuman`.

---

## What's not done (yet)

This is a working fork, not a polished product. Known gaps:

- **OAuth client IDs are build-time constants.** A fresh user has to rebuild the binary to use their own GCP / GitHub OAuth client. Moving them to runtime config is on the todo list.
- **The Google OAuth client ships unverified.** Google's verification process for a personal-use desktop app is heavyweight; you'll hit the "this app isn't verified" screen and have to click through. Add yourself as a test user on the OAuth consent screen to skip the worst friction.
- **Settings → Connections → Google tile is still a `comingSoon: true` stub.** The actual Google OAuth path is through the per-toolkit Manage dialog. Either remove the tile or wire it as a deeplink to the Manage dialog — listed in the next pass.
- **Telemetry, observability, error reporting** — Sentry is still wired up for crash reporting. Disable in `app/src/utils/config.ts` if you don't want any third-party reporting.
- **Distribution** — no signed releases, no auto-update. Build locally or set up your own release pipeline.
- If you hit something that's broken, check the logs first, then `tasks/todo.md` for known issues, then file an issue against this fork (not upstream — upstream can't fix what's specific to the closedhuman path).

  ***

## License + attribution

closedhuman is licensed under the **GNU General Public License v3.0** (GPLv3) — same as the upstream `tinyhumansai/openhuman` project it forks. The full license text is in [`LICENSE`](LICENSE). In short: you can run, study, share, and modify the code, but any redistributed version (including modifications) must also be GPLv3 and ship the source. There is no closed-source re-license path: the "closed" in *closedhuman* refers to closing the loop on the OpenHuman product backend, not to closing the source.

All credit for the original architecture, design language, and the parts of the codebase still standing goes to the OpenHuman team. The criticism above is criticism of specific implementation choices, not of the project or its authors — building a hosted-tier business model and an open-source product on the same codebase is a genuine tension, and the fork resolves it by simply dropping the hosted side.

PRs welcome against the fork's own `main` branch.
