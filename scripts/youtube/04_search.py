# /// script
# requires-python = ">=3.10"
# dependencies = ["httpx>=0.27", "python-dotenv>=1.0"]
# ///
"""Search YouTube and print top results.

NOTE: search.list is the **most expensive** common endpoint: 100 quota
units per call. At the default 10k/day quota, that's 100 searches/day.

Usage:
  uv run scripts/youtube/04_search.py "rust programming"
  uv run scripts/youtube/04_search.py "music video" --type video --max 5
"""

from __future__ import annotations

import argparse

from common import yt_get


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("query")
    parser.add_argument("--type", choices=["video", "channel", "playlist"], default="video")
    parser.add_argument("--max", type=int, default=10, help="1..50")
    args = parser.parse_args()

    data = yt_get("/search", {
        "part": "snippet",
        "q": args.query,
        "type": args.type,
        "maxResults": args.max,
    })
    if "error" in data:
        from common import pretty
        raise SystemExit(pretty(data))

    items = data.get("items", [])
    print(f"Top {len(items)} {args.type} results for {args.query!r}:\n")
    for i, item in enumerate(items, start=1):
        s = item["snippet"]
        id_ = item["id"]
        if args.type == "video":
            link = f"https://youtu.be/{id_['videoId']}"
        elif args.type == "channel":
            link = f"https://www.youtube.com/channel/{id_['channelId']}"
        else:
            link = f"https://www.youtube.com/playlist?list={id_['playlistId']}"
        print(f"  {i:2}. {s['title']}")
        print(f"      by {s['channelTitle']}  ({s['publishedAt'][:10]})")
        print(f"      {link}")
        print()


if __name__ == "__main__":
    main()
