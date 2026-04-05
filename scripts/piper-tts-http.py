#!/usr/bin/env python3
"""
Piper TTS HTTP gateway: speech synthesis + voice discovery.
Endpoints:
  GET  /health  — service status
  GET  /voices  — available voice models
  POST /tts     — synthesize text to audio
"""
import glob
import json
import os
import re
import subprocess
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

PIPER = os.environ.get("PIPER_BIN", "/opt/relay/lib/piper/piper")
MODEL = os.environ.get("PIPER_MODEL", "")
PORT = int(os.environ.get("PIPER_HTTP_PORT", "5590"))
MODELS_DIR = os.environ.get("PIPER_MODELS_DIR", "/opt/relay/lib/piper/models")


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

    def do_GET(self):
        if self.path == "/health":
            ok = os.path.isfile(PIPER) and os.path.isfile(MODEL)
            self.send_response(200 if ok else 503)
            self.send_header("Content-Type", "application/json")
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
        self.end_headers()

    def do_POST(self):
        if self.path != "/tts":
            self.send_response(404)
            self.end_headers()
            return
        try:
            n = int(self.headers.get("Content-Length", "0"))
            body = json.loads(self.rfile.read(n) or b"{}")
            text = (body.get("text") or "").strip()
            if not text or len(text) > 8000:
                self.send_response(400)
                self.end_headers()
                return
        except Exception:
            self.send_response(400)
            self.end_headers()
            return
        try:
            proc = subprocess.run(
                [PIPER, "--model", MODEL, "--output_file", "-"],
                input=text.encode(),
                capture_output=True,
                timeout=120,
            )
            if proc.returncode != 0:
                self.send_response(502)
                self.end_headers()
                return
            wav = proc.stdout
            self.send_response(200)
            self.send_header("Content-Type", "audio/wav")
            self.send_header("Content-Length", str(len(wav)))
            self.end_headers()
            self.wfile.write(wav)
        except Exception as e:
            sys.stderr.write("piper error: %s\n" % e)
            self.send_response(500)
            self.end_headers()


if __name__ == "__main__":
    if not MODEL or not os.path.isfile(MODEL):
        sys.stderr.write("PIPER_MODEL must point to an .onnx file\n")
        sys.exit(1)
    HTTPServer(("0.0.0.0", PORT), Handler).serve_forever()
