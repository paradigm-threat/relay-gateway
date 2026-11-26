#!/usr/bin/env python3
"""
Piper TTS HTTP gateway: speech synthesis + voice discovery.
Endpoints:
  GET  /health  — service status
  GET  /voices  — available voice models
  POST /tts     — synthesize text to audio
"""
import glob
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
import time
from http.server import BaseHTTPRequestHandler, HTTPServer

PIPER = os.environ.get("PIPER_BIN", "/opt/relay/lib/piper/piper")
MODEL = os.environ.get("PIPER_MODEL", "")
PORT = int(os.environ.get("PIPER_HTTP_PORT", "5590"))
MODELS_DIR = os.environ.get("PIPER_MODELS_DIR", "/opt/relay/lib/piper/models")
CACHE_DIR = os.environ.get("PIPER_CACHE_DIR", "/opt/relay/cache/piper-tts")
CACHE_TTL_SECONDS = max(0, int(os.environ.get("PIPER_CACHE_TTL_SECONDS", "86400")))
CACHE_MAX_FILES = max(0, int(os.environ.get("PIPER_CACHE_MAX_FILES", "1000")))


def ensure_cache_dir(cache_dir=CACHE_DIR):
    os.makedirs(cache_dir, exist_ok=True)


def cache_key(text, model_path, rate):
    payload = json.dumps(
        {
            "text": text,
            "model": os.path.basename(model_path or ""),
            "rate": round(float(rate or 1.0), 3),
        },
        ensure_ascii=True,
        sort_keys=True,
    )
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()


def cache_path_for_key(key, cache_dir=CACHE_DIR):
    return os.path.join(cache_dir, f"{key}.wav")


def prune_cache(cache_dir=CACHE_DIR, ttl_seconds=CACHE_TTL_SECONDS, max_files=CACHE_MAX_FILES):
    try:
        entries = []
        now = time.time()
        for name in os.listdir(cache_dir):
            if not name.endswith(".wav"):
                continue
            full = os.path.join(cache_dir, name)
            try:
                stat = os.stat(full)
            except OSError:
                continue
            age = now - stat.st_mtime
            if ttl_seconds > 0 and age > ttl_seconds:
                try:
                    os.unlink(full)
                except OSError:
                    pass
                continue
            entries.append((stat.st_mtime, full))
        if max_files > 0 and len(entries) > max_files:
            entries.sort(key=lambda item: item[0])
            for _, full in entries[: len(entries) - max_files]:
                try:
                    os.unlink(full)
                except OSError:
                    pass
    except OSError:
        pass


def get_cached_audio(key, cache_dir=CACHE_DIR, ttl_seconds=CACHE_TTL_SECONDS):
    if ttl_seconds <= 0:
        return None
    path = cache_path_for_key(key, cache_dir)
    if not os.path.isfile(path):
        return None
    try:
        stat = os.stat(path)
        if time.time() - stat.st_mtime > ttl_seconds:
            try:
                os.unlink(path)
            except OSError:
                pass
            return None
        return path, stat.st_size
    except OSError:
        return None


def put_cached_audio(key, wav, cache_dir=CACHE_DIR):
    ensure_cache_dir(cache_dir)
    target = cache_path_for_key(key, cache_dir)
    fd, tmp_path = tempfile.mkstemp(prefix="piper-cache-", suffix=".wav", dir=cache_dir)
    try:
        with os.fdopen(fd, "wb") as f:
            f.write(wav)
        os.replace(tmp_path, target)
    except OSError:
        try:
            os.unlink(tmp_path)
        except OSError:
            pass
        return
    prune_cache(cache_dir)


def title_case(value):
    return " ".join(part.capitalize() for part in value.replace("_", " ").split())


def display_name(lang, voice_name, quality):
    region = lang.split("_", 1)[1] if "_" in lang else lang
    return f"{title_case(voice_name)} ({region}, {quality})"


def model_path_for_voice_id(voice_id, models_dir=MODELS_DIR):
    if not voice_id:
        return None
    candidate = os.path.join(models_dir, f"{voice_id}.onnx")
    if os.path.isfile(candidate):
        return candidate
    return None


def discover_voices(models_dir=MODELS_DIR):
    """Scan models directory and return list of available voices."""
    voices = []
    if not os.path.isdir(models_dir):
        return voices

    for onnx_file in sorted(glob.glob(os.path.join(models_dir, "*.onnx"))):
        filename = os.path.basename(onnx_file)
        # Parse filename: en_US-lessac-medium.onnx -> language, voice, quality
        match = re.match(r"^([a-z]{2}_[A-Z]{2})-([a-z_]+)-([a-z]+)\.onnx$", filename)
        if not match:
            continue

        lang, voice_name, quality = match.groups()
        voice_id = f"{lang}-{voice_name}-{quality}"

        # Try to load metadata from .onnx.json
        metadata_file = onnx_file + ".json"
        metadata = {}
        if os.path.isfile(metadata_file):
            try:
                with open(metadata_file, "r", encoding="utf-8") as f:
                    metadata = json.load(f)
            except Exception:
                pass

        voices.append(
            {
                "id": voice_id,
                "name": display_name(lang, voice_name, quality),
                "lang": lang,
                "language": lang,
                "voice": voice_name,
                "quality": quality,
                "model_file": filename,
                "espeak_voice": metadata.get("espeak_voice", lang),
                "sample_rate": metadata.get("sample_rate", 22050),
            }
        )

    return voices


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        sys.stderr.write("%s\n" % (args[0],))

    def _cors_headers(self):
        # Browser TTS from Vercel (thirdstory.site, *.vercel.app) is cross-origin.
        origin = (self.headers.get("Origin") or "").strip() or "*"
        self.send_header("Access-Control-Allow-Origin", origin)
        self.send_header("Access-Control-Allow-Methods", "GET, POST, OPTIONS, HEAD")
        self.send_header(
            "Access-Control-Allow-Headers",
            "Content-Type, Authorization, Cache-Control",
        )
        self.send_header("Access-Control-Max-Age", "86400")
        self.send_header("Vary", "Origin")

    def do_OPTIONS(self):
        self.send_response(204)
        self._cors_headers()
        self.end_headers()

    def do_HEAD(self):
        if self.path in ("/health", "/voices"):
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self._cors_headers()
            self.end_headers()
            return
        self.send_response(404)
        self._cors_headers()
        self.end_headers()

    def do_GET(self):
        if self.path == "/health":
            ok = os.path.isfile(PIPER) and os.path.isfile(MODEL)
            self.send_response(200 if ok else 503)
            self.send_header("Content-Type", "application/json")
            self._cors_headers()
            self.end_headers()
            self.wfile.write(
                json.dumps(
                    {"ok": ok, "piper": PIPER, "model": MODEL}
                ).encode()
            )
            return
        elif self.path == "/voices":
            voices = discover_voices()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self._cors_headers()
            self.end_headers()
            self.wfile.write(
                json.dumps(
                    {
                        "voices": voices,
                        "defaultVoiceId": os.path.basename(MODEL)
                        .replace(".onnx", "")
                        if os.path.isfile(MODEL)
                        else None,
                    }
                ).encode()
            )
            return
        self.send_response(404)
        self._cors_headers()
        self.end_headers()

    def do_POST(self):
        if self.path != "/tts":
            self.send_response(404)
            self._cors_headers()
            self.end_headers()
            return
        try:
            n = int(self.headers.get("Content-Length", "0"))
            body = json.loads(self.rfile.read(n) or b"{}")
            text = (body.get("text") or "").strip()
            voice_id = (body.get("voiceId") or "").strip()
            rate = body.get("rate") or 1.0
            if not text or len(text) > 8000:
                self.send_response(400)
                self._cors_headers()
                self.end_headers()
                return
        except Exception:
            self.send_response(400)
            self._cors_headers()
            self.end_headers()
            return
        try:
            model_path = MODEL
            if voice_id:
                model_path = model_path_for_voice_id(voice_id)
                if not model_path:
                    self.send_response(400)
                    self.send_header("Content-Type", "application/json")
                    self._cors_headers()
                    self.end_headers()
                    self.wfile.write(
                        json.dumps(
                            {
                                "error": f"Requested Piper voice is not installed: {voice_id}",
                                "availableVoiceIds": [voice["id"] for voice in discover_voices()],
                                "defaultVoiceId": os.path.basename(MODEL).replace(".onnx", "") if os.path.isfile(MODEL) else None,
                            }
                        ).encode()
                    )
                    return
            cache_key_value = cache_key(text, model_path, rate)
            cached = get_cached_audio(cache_key_value)
            if cached:
                cached_path, cached_size = cached
                self.send_response(200)
                self.send_header("Content-Type", "audio/wav")
                self.send_header("Content-Length", str(cached_size))
                self.send_header("Cache-Control", f"public, max-age={CACHE_TTL_SECONDS}")
                self.send_header("X-Cache", "HIT")
                self._cors_headers()
                self.end_headers()
                with open(cached_path, "rb") as f:
                    self.wfile.write(f.read())
                return
            proc = subprocess.run(
                [PIPER, "--model", model_path, "--output_file", "-"],
                input=text.encode(),
                capture_output=True,
                timeout=120,
            )
            if proc.returncode != 0:
                self.send_response(502)
                self._cors_headers()
                self.end_headers()
                return
            wav = proc.stdout
            if wav and CACHE_TTL_SECONDS > 0:
                put_cached_audio(cache_key_value, wav)
            self.send_response(200)
            self.send_header("Content-Type", "audio/wav")
            self.send_header("Content-Length", str(len(wav)))
            self.send_header("Cache-Control", f"public, max-age={CACHE_TTL_SECONDS}")
            self.send_header("X-Cache", "MISS")
            self._cors_headers()
            self.end_headers()
            self.wfile.write(wav)
        except Exception as e:
            sys.stderr.write("piper error: %s\n" % e)
            self.send_response(500)
            self._cors_headers()
            self.end_headers()


if __name__ == "__main__":
    if not MODEL or not os.path.isfile(MODEL):
        sys.stderr.write("PIPER_MODEL must point to an .onnx file\n")
        sys.exit(1)
    ensure_cache_dir()
    prune_cache()
    HTTPServer(("0.0.0.0", PORT), Handler).serve_forever()
