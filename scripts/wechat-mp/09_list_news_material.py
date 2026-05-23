# /// script
# requires-python = ">=3.10"
# dependencies = ["httpx>=0.27", "python-dotenv>=1.0", "Pillow>=10.0"]
# ///
"""Fallback for individual subscription accounts: list "news" items in the
legacy material library.

When cgi-bin/freepublish/batchget returns 48001 (api unauthorized), this
endpoint sometimes still works because it queries the older material API
rather than the freepublish system.

Endpoint: POST cgi-bin/material/batchget_material
Body: {"type": "news", "offset": 0, "count": 20}

Run: uv run scripts/wechat-mp/09_list_news_material.py
"""

from __future__ import annotations

import httpx

from common import API_BASE, get_stable_token, pretty


def main() -> None:
    token = get_stable_token()
    resp = httpx.post(
        f"{API_BASE}/cgi-bin/material/batchget_material",
        params={"access_token": token},
        json={"type": "news", "offset": 0, "count": 20},
        timeout=15.0,
    )
    resp.raise_for_status()
    data = resp.json()

    if "errcode" in data and data.get("errcode", 0) != 0:
        print(f"WeChat error: {data}")
        return

    total = data.get("total_count", 0)
    items = data.get("item", [])
    print(f"Material 'news' items: {total} total, showing {len(items)}\n")

    if not items:
        print("(no news material)")
        return

    for i, item in enumerate(items, start=1):
        media_id = item.get("media_id")
        update_time = item.get("update_time")
        content = item.get("content", {})
        news_items = content.get("news_item", [])
        print(f"#{i}  media_id={media_id}  update_time={update_time}")
        for j, news in enumerate(news_items):
            title = news.get("title", "(no title)")
            url = news.get("url", "")
            author = news.get("author", "")
            print(f"     [{j}] {title}")
            print(f"         author: {author}")
            print(f"         url:    {url}")
        print()


if __name__ == "__main__":
    main()
