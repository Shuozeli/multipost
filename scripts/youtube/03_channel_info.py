# /// script
# requires-python = ">=3.10"
# dependencies = ["httpx>=0.27", "python-dotenv>=1.0"]
# ///
"""Fetch info about a YouTube channel by handle (@MrBeast), ID (UCxxx), or username.

Usage:
  uv run scripts/youtube/03_channel_info.py                       # default
  uv run scripts/youtube/03_channel_info.py '@MrBeast'
  uv run scripts/youtube/03_channel_info.py UCX6OQ3DkcsbYNE6H8uQQuVA
"""

from __future__ import annotations

import argparse

from common import pretty, yt_get


def resolve_channel(identifier: str) -> dict:
    parts = "snippet,statistics,contentDetails,brandingSettings"
    if identifier.startswith("UC") and len(identifier) == 24:
        return yt_get("/channels", {"part": parts, "id": identifier})
    if identifier.startswith("@"):
        return yt_get("/channels", {"part": parts, "forHandle": identifier})
    # treat as legacy username
    return yt_get("/channels", {"part": parts, "forUsername": identifier})


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("identifier", nargs="?", default="@MrBeast")
    args = parser.parse_args()

    data = resolve_channel(args.identifier)
    if "error" in data:
        raise SystemExit(pretty(data))
    items = data.get("items", [])
    if not items:
        raise SystemExit(f"Channel {args.identifier!r} not found.")
    ch = items[0]
    snip = ch["snippet"]
    stats = ch.get("statistics", {})
    uploads = ch.get("contentDetails", {}).get("relatedPlaylists", {}).get("uploads")
    print(f"  Channel:      {snip['title']}  (id={ch['id']})")
    print(f"  Handle:       {snip.get('customUrl', '?')}")
    print(f"  Country:      {snip.get('country', '?')}")
    print(f"  Subscribers:  {int(stats.get('subscriberCount', 0)):,}")
    print(f"  Videos:       {int(stats.get('videoCount', 0)):,}")
    print(f"  Views (total):{int(stats.get('viewCount', 0)):,}")
    print(f"  Uploads playlist: {uploads}")
    print(f"  Description (snippet):")
    print("    " + (snip.get("description", "")[:300] or "(none)"))


if __name__ == "__main__":
    main()
