# Kokoro TTS provider — installation notes

Reminder doc for the `kokoro` TTS provider added in commit `f267598e`. The
provider speaks the OpenAI Audio API shape against any local server — these
notes target [`mlx-audio`](https://github.com/Blaizzy/mlx-audio) running
Kokoro on Apple Silicon (MLX-accelerated) because that's the path Jokke
verified end-to-end. Any other OpenAI-compatible server (`kokoro-fastapi`,
LM Studio audio mode) works against the same provider with no code change.

This file will fold into the main `README.md` / `CLAUDE.md` rewrite when
the fork is pushed. Until then, treat it as the canonical install recipe.

## Why these steps look fiddly

Out-of-the-box `uv tool install mlx-audio` is not enough for the Kokoro
path. Specifically:

1. The `mlx-audio` core install does not pull the `[server]` extras
   (`fastapi`, `uvicorn`, `python-multipart`, `webrtcvad`,
   `setuptools<81`). Without them, `mlx_audio.server --help` fails with
   `ModuleNotFoundError: No module named 'uvicorn'`.
2. Kokoro needs `misaki` for text → phoneme conversion. That's a
   separate optional dep, also not in the core install.
3. `uv tool` picks the newest Python on the system. On Python 3.14,
   spaCy refuses to install — and Kokoro's English G2P pipeline needs
   spaCy. Pin to 3.12.
4. At runtime the Kokoro pipeline tries to `uv pip install` the spaCy
   `en_core_web_sm` model into its own venv. The subprocess sees no
   active venv → install fails. Pre-install the model so the runtime
   self-install never fires.

Tracked upstream as
[Blaizzy/mlx-audio#452](https://github.com/Blaizzy/mlx-audio/issues/452).

## Install — one command

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

Then pre-install the spaCy English model into the tool's venv (do this on
**one line** — terminal soft-wrap mangles long URLs with backslash
continuations):

```bash
URL='https://github.com/explosion/spacy-models/releases/download/en_core_web_sm-3.8.0/en_core_web_sm-3.8.0-py3-none-any.whl'
uv pip install --python ~/.local/share/uv/tools/mlx-audio/bin/python "$URL"
```

For non-English Kokoro voices add the matching `misaki` extra to the tool
install: `misaki[ja]` (Japanese), `misaki[zh]` (Mandarin). European voices
(`ef_*` Spanish, `ff_*` French) fall back to bundled espeak-ng — base
`misaki` is enough.

## Run

```bash
mlx_audio.server --host 127.0.0.1 --port 8880 \
  --realtime-model mlx-community/Kokoro-82M-bf16
```

First boot downloads ~400 MB of Kokoro weights from Hugging Face into
`~/.cache/huggingface/`. Subsequent boots are instant.

The `setuptools<81` pin will surface a `pkg_resources is deprecated`
warning at startup — that's expected and harmless; `webrtcvad` still uses
the deprecated API.

## Smoke-test the HTTP layer

```bash
curl -sS http://127.0.0.1:8880/v1/audio/speech \
  -H 'Content-Type: application/json' \
  -d '{"model":"kokoro","input":"hello from kokoro","voice":"af_bella","response_format":"wav"}' \
  --output /tmp/k.wav && afplay /tmp/k.wav
```

You should hear the line. If you get a non-200 or silence, fix the server
before touching the app — every error past this point is in the desktop
side.

## Point the app at it

Settings → Voice → **Text-to-Speech Provider** → `Kokoro (local
OpenAI-compatible server)`. The three inputs that appear default to the
right values for the recipe above:

| Field | Value |
| --- | --- |
| Endpoint | `http://localhost:8880` |
| Model | `mlx-community/Kokoro-82M-bf16` |
| Default voice | `af_bella` |

The model field must match the id mlx-audio was launched with
(`--realtime-model …`). kokoro-fastapi users can use the shorthand
`kokoro` instead — both work since the field is just passed through.

Save by tabbing out of each field (`onBlur` persists). The mascot's
spoken-reply path (`voice_reply_synthesize`) routes through the same
factory dispatch, so picking Kokoro here switches every TTS surface in
the app — no separate mascot wiring needed.

## Verify the pipeline is hot

While testing, tail the core log for the provider dispatch lines:

```bash
tail -F ~/.openhuman/logs/openhuman.$(date +%Y-%m-%d).log \
  | grep -E "voice-tts|voice-factory|kokoro"
```

Expected sequence on a successful synth:

```text
[voice-factory] create_tts_provider provider=kokoro voice=af_bella
[voice-factory] kokoro TTS dispatch endpoint=http://localhost:8880 …
[voice-tts] kokoro POST url=http://localhost:8880/v1/audio/speech model=kokoro voice=af_bella chars=…
[voice-tts] kokoro synthesized wav_bytes=N visemes=2 elapsed_ms=…
```

If you see `cloud TTS dispatch` instead, the provider didn't get persisted
— check `~/.openhuman/config.toml`: under `[local_ai]` you should see
`tts_provider = "kokoro"`.

## Caveat — lip-sync is synthetic

The Kokoro server returns raw WAV with no per-phoneme alignment. The
provider derives a flat `sil → aa` viseme timeline from character count
(same approach `local_speech_piper` uses), so the mascot's mouth opens
and closes coarsely with the audio but won't actually lip-sync to
individual phonemes. The cloud (ElevenLabs) path is the only one that
returns rich alignment.

If lip-sync precision matters more than TTS quality on a specific surface
(e.g. demo-grade mascot output), leave that surface on `cloud`.

## Where the code lives

- Provider: [`src/openhuman/inference/voice/kokoro_speech.rs`](src/openhuman/inference/voice/kokoro_speech.rs)
- Factory branch + voice-id sanitization: [`src/openhuman/voice/factory.rs`](src/openhuman/voice/factory.rs)
- Config schema: [`src/openhuman/config/schema/local_ai.rs`](src/openhuman/config/schema/local_ai.rs) — see `kokoro_endpoint_url`, `kokoro_model`, `kokoro_voice`
- RPC surface: [`src/openhuman/voice/schemas.rs`](src/openhuman/voice/schemas.rs) — `voice_set_providers` accepts `kokoro_*`, `voice_status` echoes them
- Settings UI: [`app/src/components/settings/panels/VoicePanel.tsx`](app/src/components/settings/panels/VoicePanel.tsx)
