# /// script
# requires-python = ">=3.10"
# dependencies = ["httpx>=0.27", "python-dotenv>=1.0"]
# ///
"""Update a video's privacy status.

Endpoint: PUT https://www.googleapis.com/youtube/v3/videos?part=status
Cost: 50 quota units.

Usage:
  uv run scripts/youtube/10_update_privacy.py Eehb6IN0Wdc public
  uv run scripts/youtube/10_update_privacy.py Eehb6IN0Wdc unlisted
  uv run scripts/youtube/10_update_privacy.py Eehb6IN0Wdc private
"""

from __future__ import annotations

import argparse
import json

import httpx

from common import API_BASE, get_access_token, pretty


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("video_id")
    parser.add_argument("privacy", choices=["private", "unlisted", "public"])
    args = parser.parse_args()

    body = {"id": args.video_id, "status": {"privacyStatus": args.privacy}}
    resp = httpx.put(
        f"{API_BASE}/videos",
        params={"part": "status"},
        headers={
            "Authorization": f"Bearer {get_access_token()}",
            "Content-Type": "application/json",
        },
        content=json.dumps(body).encode("utf-8"),
        timeout=15.0,
    )
    data = resp.json()
    if resp.status_code != 200 or "error" in data:
        raise SystemExit(f"HTTP {resp.status_code}\n{pretty(data)}")

    new_status = data.get("status", {})
    print(f"✓ {args.video_id} now {new_status.get('privacyStatus')!r}")
    print(f"  upload status: {new_status.get('uploadStatus')!r}")
    print(f"  url:           https://youtu.be/{args.video_id}")


if __name__ == "__main__":
    main()
