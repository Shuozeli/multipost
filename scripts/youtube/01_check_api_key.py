# /// script
# requires-python = ">=3.10"
# dependencies = ["httpx>=0.27", "python-dotenv>=1.0"]
# ///
"""Verify the YouTube API key works by looking up a known video.

Read-only. Costs 1 quota unit (out of default 10,000/day).

Run: uv run scripts/youtube/01_check_api_key.py
"""

from __future__ import annotations

from common import pretty, yt_get


# Rick Astley — Never Gonna Give You Up. Stable ID since 2009, exists in all regions.
TEST_VIDEO_ID = "dQw4w9WgXcQ"


def main() -> None:
    print(f"Looking up video {TEST_VIDEO_ID} ...")
    data = yt_get("/videos", {"part": "snippet,statistics", "id": TEST_VIDEO_ID})

    if "error" in data:
        print(pretty(data))
        raise SystemExit(f"\n✗ API call failed: {data['error']['message']}")

    items = data.get("items", [])
    if not items:
        print(pretty(data))
        raise SystemExit("✗ No items returned (video may be region-blocked here).")

    v = items[0]
    snip = v["snippet"]
    stats = v.get("statistics", {})
    print(f"  Title:        {snip['title']}")
    print(f"  Channel:      {snip['channelTitle']}")
    print(f"  Published:    {snip['publishedAt']}")
    print(f"  Views:        {int(stats.get('viewCount', 0)):,}")
    print(f"  Likes:        {int(stats.get('likeCount', 0)):,}")
    print(f"  Comments:     {int(stats.get('commentCount', 0)):,}")
    print("\n✓ API key works.")


if __name__ == "__main__":
    main()
