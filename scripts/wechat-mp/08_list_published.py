# /// script
# requires-python = ">=3.10"
# dependencies = ["httpx>=0.27", "python-dotenv>=1.0", "Pillow>=10.0"]
# ///
"""List articles that have been PUBLISHED (gone public) on the account.

Different from 03_list_drafts.py:
  - 03 reads from cgi-bin/draft/batchget   — private drafts in admin console
  - 08 reads from cgi-bin/freepublish/batchget — public articles with mp.weixin.qq.com/s/... URLs

Note: per WeChat platform limit, articles published via the API do NOT
appear on the account's homepage feed — they only exist at their permalink.

Run: uv run scripts/wechat-mp/08_list_published.py
"""

from __future__ import annotations

import argparse

import httpx

from common import API_BASE, get_stable_token, pretty


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--offset", type=int, default=0)
    parser.add_argument("--count", type=int, default=20, help="1..20")
    parser.add_argument("--with-content", action="store_true",
                        help="Include full article HTML (default: just metadata)")
    args = parser.parse_args()

    token = get_stable_token()
    resp = httpx.post(
        f"{API_BASE}/cgi-bin/freepublish/batchget",
        params={"access_token": token},
        json={
            "offset": args.offset,
            "count": args.count,
            "no_content": 0 if args.with_content else 1,
        },
        timeout=15.0,
    )
    resp.raise_for_status()
    data = resp.json()

    if "errcode" in data and data["errcode"] != 0:
        raise SystemExit(f"WeChat error: {data}")

    total = data.get("total_count", 0)
    items = data.get("item", [])
    print(f"Published articles: {total} total, showing {len(items)}\n")

    if not items:
        print("(no published articles)")
        return

    for i, item in enumerate(items, start=1):
        article_id = item.get("article_id")
        update_time = item.get("update_time")
        content = item.get("content", {})
        news_items = content.get("news_item", [])
        print(f"#{i}  article_id={article_id}  update_time={update_time}")
        for j, news in enumerate(news_items):
            title = news.get("title", "(no title)")
            url = news.get("url", "")
            author = news.get("author", "")
            print(f"     [{j}] {title}")
            print(f"         author: {author}")
            print(f"         url:    {url}")
        print()

    if args.with_content:
        print("\n--- raw response ---")
        print(pretty(data))


if __name__ == "__main__":
    main()
