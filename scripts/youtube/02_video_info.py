# /// script
# requires-python = ">=3.10"
# dependencies = ["httpx>=0.27", "python-dotenv>=1.0"]
# ///
"""Fetch full metadata for a specific YouTube video.

Usage:
  uv run scripts/youtube/02_video_info.py                  # default test video
  uv run scripts/youtube/02_video_info.py dQw4w9WgXcQ      # by ID
  uv run scripts/youtube/02_video_info.py 'https://youtu.be/dQw4w9WgXcQ'
"""

from __future__ import annotations

import argparse
import re

from common import pretty, yt_get


VIDEO_ID_RE = re.compile(r"(?:v=|youtu\.be/|/shorts/|/embed/)([A-Za-z0-9_-]{11})")


def extract_video_id(s: str) -> str:
    if len(s) == 11 and re.fullmatch(r"[A-Za-z0-9_-]{11}", s):
        return s
    m = VIDEO_ID_RE.search(s)
    if not m:
        raise SystemExit(f"Couldn't extract a video ID from {s!r}")
    return m.group(1)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("video", nargs="?", default="dQw4w9WgXcQ",
                        help="Video ID or YouTube URL")
    args = parser.parse_args()
    vid = extract_video_id(args.video)

    parts = ["snippet", "statistics", "contentDetails", "status", "topicDetails"]
    data = yt_get("/videos", {"part": ",".join(parts), "id": vid})
    if "error" in data:
        raise SystemExit(pretty(data))
    items = data.get("items", [])
    if not items:
        raise SystemExit(f"Video {vid} not found or unavailable in this region.")
    print(pretty(items[0]))


if __name__ == "__main__":
    main()
