# /// script
# requires-python = ">=3.10"
# dependencies = ["httpx>=0.27", "python-dotenv>=1.0", "yt-dlp>=2026.1.1"]
# ///
"""Find a Creative-Commons-licensed short on YouTube and download it.

Uses the YouTube Data API v3 search with videoLicense=creativeCommon and
videoDuration=short (< 4 min). Picks the first result. Downloads via the
yt-dlp library.

NOTE on yt-dlp vs yt-dlp-rs: we use plain `yt-dlp` (Python lib) here for
speed. The Shuozeli `yt-dlp-rs` is a gRPC wrapper around the same engine
and would give us the same file — it just needs `docker compose up` first.

Run: uv run scripts/youtube/09_find_and_download_cc.py
     uv run scripts/youtube/09_find_and_download_cc.py --query "nasa"
"""

from __future__ import annotations

import argparse
from pathlib import Path

import yt_dlp

from common import SCRIPT_DIR, pretty, yt_get


DOWNLOADS_DIR = SCRIPT_DIR / "downloads"
DOWNLOADS_DIR.mkdir(exist_ok=True)


def find_cc_short(query: str, max_results: int = 5) -> list[dict]:
    data = yt_get("/search", {
        "part": "snippet",
        "q": query,
        "type": "video",
        "maxResults": max_results,
        "videoLicense": "creativeCommon",
        "videoDuration": "short",
        "videoEmbeddable": "true",
    })
    if "error" in data:
        raise SystemExit(pretty(data))
    return data.get("items", [])


def download(video_id: str) -> Path:
    url = f"https://youtu.be/{video_id}"
    out_template = str(DOWNLOADS_DIR / "%(id)s.%(ext)s")
    opts = {
        "format": "best[height<=480][ext=mp4]/best[height<=480]/best",
        "outtmpl": out_template,
        "noprogress": True,
        "quiet": True,
    }
    with yt_dlp.YoutubeDL(opts) as ydl:
        info = ydl.extract_info(url, download=True)
        # the resolved filename
        return Path(ydl.prepare_filename(info))


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--query", default="nature timelapse",
                        help="Search query (default: nature timelapse)")
    parser.add_argument("--max", type=int, default=5,
                        help="How many candidates to surface (default: 5)")
    args = parser.parse_args()

    print(f"Searching CC-licensed shorts for {args.query!r} ...")
    candidates = find_cc_short(args.query, args.max)
    if not candidates:
        raise SystemExit("No CC shorts found for that query.")

    for i, c in enumerate(candidates):
        s = c["snippet"]
        print(f"  [{i}] {s['title']}  ({s['publishedAt'][:10]})")
        print(f"      by {s['channelTitle']}")
        print(f"      https://youtu.be/{c['id']['videoId']}")
    print()

    pick = candidates[0]
    vid = pick["id"]["videoId"]
    title = pick["snippet"]["title"]
    channel = pick["snippet"]["channelTitle"]
    print(f"Picked #0: {title!r} from {channel}")
    print(f"Downloading https://youtu.be/{vid} into {DOWNLOADS_DIR} ...")
    path = download(vid)
    print(f"  ✓ saved: {path}")
    print(f"  size:    {path.stat().st_size:,} bytes ({path.stat().st_size / 1_048_576:.2f} MB)")
    print(f"\nNext: upload with")
    print(f"  uv run scripts/youtube/08_upload_video.py \\")
    print(f"    --file '{path}' \\")
    print(f"    --title 'Re-upload test: {title[:40]}' \\")
    print(f"    --description 'Source: youtu.be/{vid} ({channel}, CC-BY). multipost upload test.'")


if __name__ == "__main__":
    main()
