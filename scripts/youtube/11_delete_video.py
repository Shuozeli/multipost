# /// script
# requires-python = ">=3.10"
# dependencies = ["httpx>=0.27", "python-dotenv>=1.0"]
# ///
"""Delete a video from the authenticated channel.

Endpoint: DELETE https://www.googleapis.com/youtube/v3/videos?id=<id>
Cost: 50 quota units.

DESTRUCTIVE AND IRREVERSIBLE. Confirms before firing unless --yes is passed.

Usage:
  uv run scripts/youtube/11_delete_video.py Eehb6IN0Wdc          # interactive confirm
  uv run scripts/youtube/11_delete_video.py Eehb6IN0Wdc --yes    # no prompt
"""

from __future__ import annotations

import argparse

import httpx

from common import API_BASE, get_access_token


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("video_id")
    parser.add_argument("--yes", action="store_true",
                        help="Skip interactive confirmation")
    args = parser.parse_args()

    if not args.yes:
        answer = input(f"Delete video {args.video_id}? IRREVERSIBLE. Type 'yes' to confirm: ")
        if answer.strip().lower() != "yes":
            raise SystemExit("Aborted.")

    resp = httpx.delete(
        f"{API_BASE}/videos",
        params={"id": args.video_id},
        headers={"Authorization": f"Bearer {get_access_token()}"},
        timeout=15.0,
    )
    if resp.status_code == 204:
        print(f"✓ {args.video_id} deleted")
        return
    print(f"HTTP {resp.status_code}")
    print(resp.text)
    raise SystemExit(1)


if __name__ == "__main__":
    main()
