# Piper TTS Installation & Voice Discovery

## Overview

Piper TTS is now fully integrated into relay-server with automatic voice discovery via the `/voices` endpoint. This setup enables paradigm-threat-site and other clients to discover and use available text-to-speech models.

## What Changed

### 1. Enhanced `piper-tts-http.py` Wrapper

The wrapper now implements three endpoints:

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/health` | GET | Service health check |
| `/voices` | GET | Discover available voice models (NEW) |
| `/tts` | POST | Synthesize text to audio |

**GET `/voices` Response:**
```json
{
  "voices": [
    {
      "id": "en_US-lessac-medium",
      "language": "en_US",
      "voice": "lessac",
      "quality": "medium",
      "model_file": "en_US-lessac-medium.onnx",
      "espeak_voice": "en_US",
      "sample_rate": 22050
    }
  ],
  "defaultVoiceId": "en_US-lessac-medium"
}
```

### 2. Systemd Service Configuration

The `relay-tts-piper.service` now includes the `PIPER_MODELS_DIR` environment variable:

```ini
[Service]
...
Environment=PIPER_BIN=/opt/relay/lib/piper/piper
Environment=PIPER_MODEL=/opt/relay/lib/piper/models/en_GB-cori-medium.onnx
Environment=PIPER_MODELS_DIR=/opt/relay/lib/piper/models
Environment=PIPER_HTTP_PORT=5590
Environment=PIPER_CACHE_DIR=/opt/relay/cache/piper-tts
Environment=PIPER_CACHE_TTL_SECONDS=86400
Environment=PIPER_CACHE_MAX_FILES=1000
...
```

This allows the `/voices` endpoint to discover all installed models automatically, while `PIPER_MODEL` determines the server default voice.

### 3. Automatic Installation & Updates

When TTS is enabled during `./install.sh install`, `./install.sh update`, or `./install.sh reconfigure-features`:

1. **Enhanced wrapper** is copied to `/opt/relay/bin/piper-tts-http.py`
2. **Models directory** is created and populated with the selected voices
3. **Systemd service** is configured with proper environment variables and the chosen default voice
4. **Service is enabled** and automatically started
5. **Shared libraries** are validated and repaired if Piper is incomplete
6. **Identical synthesis responses** are cached on disk (default TTL: 1 day)

## Installation

### Non-Interactive Installation

```bash
# Enable Piper TTS during install
RELAY_FEAT_PIPER=1 RELAY_INSTALL_NONINTERACTIVE=1 sudo -E ./install.sh install

# Explicit voice list + default
RELAY_FEAT_PIPER=1 \
RELAY_FEAT_PIPER_VOICES="en_GB-cori-medium en_US-lessac-medium" \
RELAY_FEAT_PIPER_DEFAULT_VOICE="en_GB-cori-medium" \
RELAY_FEAT_PIPER_CACHE_TTL_SECONDS=86400 \
RELAY_INSTALL_NONINTERACTIVE=1 \
sudo -E ./install.sh install

# Or reconfigure existing install
RELAY_FEAT_PIPER=1 RELAY_INSTALL_NONINTERACTIVE=1 sudo -E ./install.sh reconfigure-features

# Update to latest version
./install.sh update
```

### Interactive Installation

```bash
sudo ./install.sh install
# Prompts:
#   "Enable Piper TTS (HTTP on port 5590)? [y/N]"
#   "Voice ids to install..." (example: en_GB-cori-medium en_US-lessac-medium)
#   "Default Piper voice id..." (example: en_GB-cori-medium)
```

## Voice Model Management

## Audio Response Cache

The relay Piper wrapper caches synthesized WAV output on disk by request hash.
Identical `POST /tts` calls with the same text, selected voice model, and rate will reuse the cached file until the TTL expires.

Defaults:

- `PIPER_CACHE_DIR=/opt/relay/cache/piper-tts`
- `PIPER_CACHE_TTL_SECONDS=86400` (1 day)
- `PIPER_CACHE_MAX_FILES=1000`

Useful install-time overrides:

```bash
RELAY_FEAT_PIPER_CACHE_TTL_SECONDS=3600
RELAY_FEAT_PIPER_CACHE_MAX_FILES=2000
```

Responses include `X-Cache: HIT|MISS` for verification.

### Adding More Voice Models

1. Download `.onnx` files from [Piper Voices](https://huggingface.co/rhasspy/piper-voices):

```bash
cd /opt/relay/lib/piper/models

# Download a model (example: GB Cori)
curl -fsSL https://huggingface.co/rhasspy/piper-voices/resolve/v1.0.0/en/en_GB/cori/medium/en_GB-cori-medium.onnx \
  -o en_GB-cori-medium.onnx

# Optional: Download metadata
curl -fsSL https://huggingface.co/rhasspy/piper-voices/resolve/v1.0.0/en/en_GB/cori/medium/en_GB-cori-medium.onnx.json \
  -o en_GB-cori-medium.onnx.json
```

2. Refresh the feature inventory:

```bash
sudo ./install.sh refresh-features
```

3. Verify the new voice appears in `/voices`:

```bash
curl http://localhost:5590/voices | jq .voices[].id
```

## Performance Considerations

### Memory Usage

| Server Plan | RAM | TTS Baseline | Available |
|------------|-----|-------------|-----------|
| Nanode 1GB | 961 MB | 9.4 MB | ~650 MB | 
| Linode 2GB | 1.9 GB | 9.4 MB | ~1.6 GB |

**Recommendation:** 
- **Nanode 1GB**: TTS works but with tight memory margins. Synthesis requests may spill to swap (slow).
- **Linode 2GB+**: Comfortable; synthesis requests are responsive.

### Synthesis Performance

- **Single request**: ~1-2 seconds for typical text
- **Concurrent requests**: Limited to 1 per server (single CPU). Requests queue on busy nodes.
- **Timeout**: 120 seconds per `/tts` request

## Client Configuration

### paradigm-threat-site

The app fetches voices on startup via:

```
GET http://iad1.relaygateway.net:5590/voices
GET http://ord1.relaygateway.net:5590/voices
GET http://hkg1.relaygateway.net:5590/voices
```

Configured in `.env`:

```bash
PIPER_API_URLS=http://iad1.relaygateway.net:5590 http://ord1.relaygateway.net:5590 http://hkg1.relaygateway.net:5590
```

If one server is unavailable, the app falls back to the others.

### Other Clients

To implement TTS discovery in a custom app:

1. Fetch `/voices` from each TTS server
2. Merge results by voice ID (first wins)
3. Synthesize via `POST /tts` with JSON `{"text": "...", ...}`

## Troubleshooting

### `/voices` returns empty list

**Cause:** No `.onnx` files found in `PIPER_MODELS_DIR`.

**Fix:**
```bash
ls -lh /opt/relay/lib/piper/models/*.onnx
# Should list at least: en_US-lessac-medium.onnx

# If missing, reinstall:
sudo ./install.sh reconfigure-features
```

### Service fails to start

**Check logs:**
```bash
sudo journalctl -u relay-tts-piper -n 50
```

**Common issues:**
- `PIPER_MODEL` file doesn't exist → ensure `.onnx` at expected path
- Python3 not available → `sudo apt install python3`
- Port 5590 in use → change `RELAY_PIPER_HTTP_PORT` before install

### Synthesis is slow

**On Nanode 1GB:**
- Check memory: `free -h`
- If swap is high, synthesis is disk-bound (slow)
- **Solution:** add more voice nodes or upgrade plan

**On any server:**
- Check for concurrent requests: `systemctl status relay-tts-piper`
- Synthesize requests are sequential (single thread)
- Keep requests < 8000 characters


## Files Updated

- `scripts/piper-tts-http.py` — Enhanced with `/voices` endpoint
- `scripts/relay-install.sh` — Updated systemd service config

## References

- [Piper TTS GitHub](https://github.com/rhasspy/piper/)
- [Piper Voices HuggingFace](https://huggingface.co/rhasspy/piper-voices)
- [INSTALL_FEATURES.md](./INSTALL_FEATURES.md) — Feature configuration docs
