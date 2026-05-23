# /// script
# requires-python = ">=3.10"
# dependencies = ["python-dotenv>=1.0"]
# ///
"""Shared helpers for the Twitter pwright-CLI prototype.

We drive Shuozeli's `pwright` Rust CLI as a subprocess. State (the WS URL,
the active tab ID) persists across runs via `.pwright/state.json` in this
script directory.

Why not playwright-python: per design doc §8, we want persistent profile
per account, which means attaching to a real Chrome with the user's actual
session — not creating an ephemeral context. The Chrome at `CDP_URL`
(see `.env`) should already be logged into Twitter.
"""

from __future__ import annotations

import os
import subprocess
from dataclasses import dataclass
from pathlib import Path

from dotenv import load_dotenv

SCRIPT_DIR = Path(__file__).resolve().parent
ENV_PATH = SCRIPT_DIR / ".env"
SCREENSHOT_DIR = SCRIPT_DIR / "screenshots"
SCREENSHOT_DIR.mkdir(exist_ok=True)

load_dotenv(ENV_PATH)

PWRIGHT_BIN = Path(
    os.environ.get("PWRIGHT_BIN", "pwright"),  # falls back to whatever is on PATH
)


def _cdp_url() -> str:
    url = os.environ.get("CDP_URL")
    if not url:
        raise SystemExit(f"CDP_URL not set; populate {ENV_PATH}")
    return url


@dataclass
class PwrightResult:
    returncode: int
    stdout: str
    stderr: str

    def ok(self) -> bool:
        return self.returncode == 0


def pwright(*args: str, check: bool = True, timeout: int = 30) -> PwrightResult:
    """Run the pwright CLI with PWRIGHT_CDP set and cwd at scripts/twitter/.

    cwd matters: pwright writes session state to ./.pwright/state.json.
    """
    env = {**os.environ, "PWRIGHT_CDP": _cdp_url()}
    proc = subprocess.run(
        [str(PWRIGHT_BIN), *args],
        cwd=str(SCRIPT_DIR),
        env=env,
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    result = PwrightResult(proc.returncode, proc.stdout, proc.stderr)
    if check and not result.ok():
        raise RuntimeError(
            f"pwright {' '.join(args)} failed (exit={result.returncode})\n"
            f"--- stdout ---\n{result.stdout}\n--- stderr ---\n{result.stderr}"
        )
    return result


def screenshot_to(name: str) -> Path:
    """Run `pwright screenshot` and move the result into screenshots/<name>."""
    res = pwright("screenshot")
    # pwright prints "[ok] Screenshot saved: <path>"
    for line in res.stdout.splitlines():
        if "Screenshot saved:" in line:
            src = SCRIPT_DIR / line.split("Screenshot saved:")[1].strip().split()[0]
            dst = SCREENSHOT_DIR / name
            src.rename(dst)
            return dst
    raise RuntimeError(f"could not find screenshot path in:\n{res.stdout}")
