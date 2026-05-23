# /// script
# requires-python = ">=3.10"
# dependencies = ["playwright>=1.49", "python-dotenv>=1.0", "httpx>=0.27"]
# ///
"""Shared helpers for the Toutiao pwright prototype.

Same CDP-WS pattern as scripts/{douyin,twitter}/common.py.
"""

from __future__ import annotations

import os
from pathlib import Path
from urllib.parse import urlparse

import httpx
from dotenv import load_dotenv

SCRIPT_DIR = Path(__file__).resolve().parent
ENV_PATH = SCRIPT_DIR / ".env"
SCREENSHOT_DIR = SCRIPT_DIR / "screenshots"
SCREENSHOT_DIR.mkdir(exist_ok=True)

load_dotenv(ENV_PATH)


def cdp_http_url() -> str:
    url = os.environ.get("CDP_URL")
    if not url:
        raise SystemExit(f"CDP_URL not set; populate {ENV_PATH}")
    return url.rstrip("/")


def cdp_ws_url() -> str:
    http_url = cdp_http_url()
    info = httpx.get(f"{http_url}/json/version", timeout=5.0).json()
    ws = info["webSocketDebuggerUrl"]
    parsed_http = urlparse(http_url)
    rest = ws.split("/", 3)[3]
    return f"ws://{parsed_http.hostname}:{parsed_http.port}/{rest}"


def screenshot_path(name: str) -> Path:
    return SCREENSHOT_DIR / name
