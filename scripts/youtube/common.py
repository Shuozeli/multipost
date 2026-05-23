# /// script
# requires-python = ">=3.10"
# dependencies = ["httpx>=0.27", "python-dotenv>=1.0"]
# ///
"""Shared helpers for the YouTube Data API v3 prototype.

Read-only operations use a plain API key. Write operations (upload, edit,
delete) need OAuth 2.0 and live in later scripts.
"""

from __future__ import annotations

import json
import os
import time
from pathlib import Path

import httpx
from dotenv import load_dotenv

API_BASE = "https://www.googleapis.com/youtube/v3"
TOKEN_ENDPOINT = "https://oauth2.googleapis.com/token"
SCRIPT_DIR = Path(__file__).resolve().parent
ENV_PATH = SCRIPT_DIR / ".env"
TOKENS_PATH = SCRIPT_DIR / "tokens.json"

load_dotenv(ENV_PATH)


def api_key() -> str:
    k = os.environ.get("YOUTUBE_API_KEY")
    if not k or "XXX" in k:
        raise SystemExit(f"YOUTUBE_API_KEY not set in {ENV_PATH}")
    return k


def yt_get(path: str, params: dict | None = None, timeout: float = 15.0) -> dict:
    """Make a YouTube Data API v3 GET call with the API key attached.

    Returns parsed JSON. Raises on transport error; otherwise returns the
    body even if the API itself returned an error (caller checks).
    """
    full_params = {"key": api_key(), **(params or {})}
    resp = httpx.get(f"{API_BASE}{path}", params=full_params, timeout=timeout)
    return resp.json()


def pretty(obj) -> str:
    return json.dumps(obj, indent=2, ensure_ascii=False)


# ---------- OAuth 2.0 helpers ----------

def oauth_client_id() -> str:
    v = os.environ.get("YOUTUBE_OAUTH_CLIENT_ID")
    if not v:
        raise SystemExit(f"YOUTUBE_OAUTH_CLIENT_ID not set in {ENV_PATH}")
    return v


def oauth_client_secret() -> str:
    v = os.environ.get("YOUTUBE_OAUTH_CLIENT_SECRET")
    if not v:
        raise SystemExit(f"YOUTUBE_OAUTH_CLIENT_SECRET not set in {ENV_PATH}")
    return v


def oauth_port() -> int:
    return int(os.environ.get("YOUTUBE_OAUTH_PORT", "8765"))


def load_tokens() -> dict | None:
    if not TOKENS_PATH.exists():
        return None
    return json.loads(TOKENS_PATH.read_text())


def save_tokens(tokens: dict) -> None:
    # ensure expires_at is present (epoch seconds)
    if "expires_at" not in tokens and "expires_in" in tokens:
        tokens["expires_at"] = int(time.time()) + int(tokens["expires_in"]) - 30
    TOKENS_PATH.write_text(json.dumps(tokens, indent=2))


def refresh_access_token(refresh_token: str) -> dict:
    resp = httpx.post(TOKEN_ENDPOINT, data={
        "refresh_token": refresh_token,
        "client_id": oauth_client_id(),
        "client_secret": oauth_client_secret(),
        "grant_type": "refresh_token",
    }, timeout=15.0)
    resp.raise_for_status()
    data = resp.json()
    # refresh response doesn't include refresh_token; keep the old one
    data["refresh_token"] = refresh_token
    return data


def get_access_token() -> str:
    """Return a valid access token, refreshing if needed.

    Requires `tokens.json` to exist (run 05_oauth_login.py first).
    """
    tokens = load_tokens()
    if not tokens:
        raise SystemExit(f"No {TOKENS_PATH}. Run 05_oauth_login.py first.")
    if tokens.get("expires_at", 0) > time.time() + 60:
        return tokens["access_token"]
    print("  (access token expired, refreshing...)")
    new = refresh_access_token(tokens["refresh_token"])
    save_tokens(new)
    return new["access_token"]


def yt_get_oauth(path: str, params: dict | None = None, timeout: float = 15.0) -> dict:
    """OAuth-authenticated GET. Used for any endpoint that needs `mine=true`
    or other write-permission-adjacent reads."""
    headers = {"Authorization": f"Bearer {get_access_token()}"}
    resp = httpx.get(f"{API_BASE}{path}", params=params or {}, headers=headers, timeout=timeout)
    return resp.json()
