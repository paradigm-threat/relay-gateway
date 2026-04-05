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
Environment=PIPER_MODEL=/opt/relay/lib/piper/models/en_US-lessac-medium.onnx
Environment=PIPER_MODELS_DIR=/opt/relay/lib/piper/models
Environment=PIPER_HTTP_PORT=5590
...
```

This allows the `/voices` endpoint to discover all installed models automatically.

### 3. Automatic Installation & Updates

When TTS is enabled during `./install.sh install` or `./install.sh reconfigure-features`:

1. **Enhanced wrapper** is copied to `/opt/relay/bin/piper-tts-http.py`
2. **Models directory** is created and populated with default voice
3. **Systemd service** is configured with proper environment variables
4. **Service is enabled** and automatically started

## Installation

### Non-Interactive Installation

```bash
# Enable Piper TTS during install
RELAY_FEAT_PIPER=1 RELAY_INSTALL_NONINTERACTIVE=1 sudo -E ./install.sh install

# Or reconfigure existing install
RELAY_FEAT_PIPER=1 RELAY_INSTALL_NONINTERACTIVE=1 sudo -E ./install.sh reconfigure-features

# Update to latest version
./install.sh update
```

### Interactive Installation

```bash
sudo ./install.sh install
# Prompts: "Enable Piper TTS (HTTP on port 5590)? [y/N]" → answer 'y'
```

## Voice Model Management

### Adding More Voice Models

1. Download `.onnx` files from [Piper Voices](https://huggingface.co/rhasspy/piper-voices):

```bash
cd /opt/relay/lib/piper/models

# Download a model (example: Spanish voice)
curl -fsSL https://huggingface.co/rhasspy/piper-voices/resolve/v1.0.0/es/es_ES/shivam-medium.onnx \
  -o es_ES-shivam-medium.onnx

# Optional: Download metadata
curl -fsSL https://huggingface.co/rhasspy/piper-voices/resolve/v1.0.0/es/es_ES/shivam-medium.onnx.json \
  -o es_ES-shivam-medium.onnx.json
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
GET http://atlanta2.relaygateway.net:5590/voices
GET http://atlanta3.relaygateway.net:5590/voices
```

Configured in `.env`:

```bash
PIPER_API_URLS=http://atlanta2.relaygateway.net:5590 http://atlanta3.relaygateway.net:5590
```

If one server is unavailable, the app falls back to the other.

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
