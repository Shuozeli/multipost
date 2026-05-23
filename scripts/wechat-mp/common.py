# /// script
# requires-python = ">=3.10"
# dependencies = ["httpx>=0.27", "python-dotenv>=1.0", "Pillow>=10.0"]
# ///
"""Shared helpers for the WeChat MP test scripts."""

from __future__ import annotations

import json
import os
import time
from dataclasses import dataclass
from pathlib import Path

import httpx
from dotenv import load_dotenv

API_BASE = "https://api.weixin.qq.com"
SCRIPT_DIR = Path(__file__).resolve().parent
ENV_PATH = SCRIPT_DIR / ".env"
TOKEN_CACHE = SCRIPT_DIR / "token_cache.json"

load_dotenv(ENV_PATH)


@dataclass
class Credentials:
    appid: str
    secret: str
    account_name: str


def load_credentials() -> Credentials:
    appid = os.environ.get("WECHAT_MP_APPID")
    secret = os.environ.get("WECHAT_MP_SECRET")
    name = os.environ.get("WECHAT_MP_ACCOUNT_NAME", "(unnamed)")
    if not appid or not secret:
        raise SystemExit(
            f"Missing WECHAT_MP_APPID or WECHAT_MP_SECRET. Set them in {ENV_PATH}"
        )
    return Credentials(appid=appid, secret=secret, account_name=name)


def get_stable_token(force_refresh: bool = False) -> str:
    """Fetch a stable access_token. Caches to disk to avoid burning quota."""
    creds = load_credentials()

    if not force_refresh and TOKEN_CACHE.exists():
        cached = json.loads(TOKEN_CACHE.read_text())
        if cached.get("appid") == creds.appid and cached.get("expires_at", 0) > time.time() + 60:
            return cached["access_token"]

    resp = httpx.post(
        f"{API_BASE}/cgi-bin/stable_token",
        json={
            "grant_type": "client_credential",
            "appid": creds.appid,
            "secret": creds.secret,
            "force_refresh": force_refresh,
        },
        timeout=10.0,
    )
    resp.raise_for_status()
    data = resp.json()
    if "access_token" not in data:
        raise SystemExit(f"WeChat returned error: {json.dumps(data, ensure_ascii=False)}")

    TOKEN_CACHE.write_text(
        json.dumps(
            {
                "appid": creds.appid,
                "access_token": data["access_token"],
                "expires_at": int(time.time()) + int(data["expires_in"]),
            },
            ensure_ascii=False,
        )
    )
    return data["access_token"]


def pretty(obj) -> str:
    return json.dumps(obj, indent=2, ensure_ascii=False)


def make_test_image(name: str = "test-image.png") -> Path:
    """Generate a small test PNG if it doesn't exist yet."""
    path = SCRIPT_DIR / name
    if path.exists():
        return path

    from PIL import Image, ImageDraw

    img = Image.new("RGB", (900, 500), color=(255, 235, 205))
    draw = ImageDraw.Draw(img)
    draw.rectangle([(20, 20), (880, 480)], outline=(120, 60, 0), width=6)
    draw.text((60, 60), "multipost · WeChat MP integration test", fill=(60, 30, 0))
    draw.text((60, 110), "do not redistribute — generated locally", fill=(60, 30, 0))
    img.save(path, "PNG")
    return path


def load_artifacts() -> dict:
    """Load the cross-script artifacts cache (media_ids, draft_ids)."""
    path = SCRIPT_DIR / "artifacts.json"
    if not path.exists():
        return {}
    return json.loads(path.read_text())


def save_artifacts(data: dict) -> None:
    path = SCRIPT_DIR / "artifacts.json"
    path.write_text(json.dumps(data, indent=2, ensure_ascii=False))
