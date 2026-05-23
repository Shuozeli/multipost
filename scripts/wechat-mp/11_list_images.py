# /// script
# requires-python = ">=3.10"
# dependencies = ["httpx>=0.27", "python-dotenv>=1.0"]
# ///
"""List permanent images in the material library (素材库).

Read-only. Useful for picking a `thumb_media_id` to set as an article's
cover image without uploading a new file.

Endpoint: POST cgi-bin/material/batchget_material
Body: {"type": "image", "offset": 0, "count": 20}

Run: uv run scripts/wechat-mp/11_list_images.py
     uv run scripts/wechat-mp/11_list_images.py --count 50
"""

from __future__ import annotations

import argparse
import datetime as dt

import httpx

from common import API_BASE, get_stable_token, pretty


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--offset", type=int, default=0)
    parser.add_argument("--count", type=int, default=20, help="1..20")
    args = parser.parse_args()

    token = get_stable_token()

    # First, get the total count via cgi-bin/material/get_materialcount
    cnt_resp = httpx.get(
        f"{API_BASE}/cgi-bin/material/get_materialcount",
        params={"access_token": token},
        timeout=10.0,
    )
    cnt = cnt_resp.json()
    if "errcode" in cnt and cnt["errcode"] != 0:
        raise SystemExit(pretty(cnt))
    print(f"Material library totals:")
    print(f"  Images:  {cnt.get('image_count')}")
    print(f"  Voice:   {cnt.get('voice_count')}")
    print(f"  Video:   {cnt.get('video_count')}")
    print(f"  News:    {cnt.get('news_count')}")
    print()

    # Then list the images
    resp = httpx.post(
        f"{API_BASE}/cgi-bin/material/batchget_material",
        params={"access_token": token},
        json={"type": "image", "offset": args.offset, "count": args.count},
        timeout=15.0,
    )
    data = resp.json()
    if "errcode" in data and data["errcode"] != 0:
        raise SystemExit(pretty(data))

    items = data.get("item", [])
    print(f"Showing {len(items)} of {data.get('total_count', '?')} images "
          f"(offset {args.offset}):\n")
    for i, it in enumerate(items, start=args.offset):
        ts = it.get("update_time", 0)
        when = dt.datetime.fromtimestamp(ts).strftime("%Y-%m-%d %H:%M") if ts else "?"
        name = it.get("name", "(unnamed)")
        media_id = it.get("media_id", "?")
        url = it.get("url", "")
        print(f"  [{i}] {name}   uploaded {when}")
        print(f"       media_id: {media_id}")
        print(f"       url:      {url}")
        print()

    print("To use image #N as the article cover:")
    print(f"  uv run scripts/wechat-mp/12_publish_article.py ... \\")
    print(f"      --cover-media-id <media_id from above>")


if __name__ == "__main__":
    main()
