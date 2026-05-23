# /// script
# requires-python = ">=3.10"
# dependencies = ["httpx>=0.27", "python-dotenv>=1.0"]
# ///
"""Delete every draft in the MP account.

Endpoint: POST cgi-bin/draft/delete
Body:     {"media_id": "..."}

Paginates via cgi-bin/draft/batchget (20 per page).

Run: uv run scripts/wechat-mp/13_delete_all_drafts.py
"""

from __future__ import annotations

import httpx

from common import API_BASE, get_stable_token, pretty


def list_all_drafts(token: str) -> list[dict]:
    """Paginate cgi-bin/draft/batchget to get every draft."""
    all_items: list[dict] = []
    offset = 0
    while True:
        resp = httpx.post(
            f"{API_BASE}/cgi-bin/draft/batchget",
            params={"access_token": token},
            json={"offset": offset, "count": 20, "no_content": 1},
            timeout=15.0,
        )
        data = resp.json()
        if data.get("errcode", 0) != 0:
            raise SystemExit(pretty(data))
        items = data.get("item", [])
        all_items.extend(items)
        if len(items) < 20:
            break
        offset += 20
    return all_items


def delete_draft(token: str, media_id: str) -> dict:
    resp = httpx.post(
        f"{API_BASE}/cgi-bin/draft/delete",
        params={"access_token": token},
        json={"media_id": media_id},
        timeout=10.0,
    )
    return resp.json()


def main() -> None:
    token = get_stable_token()
    drafts = list_all_drafts(token)
    print(f"Found {len(drafts)} draft(s) to delete:\n")

    for i, d in enumerate(drafts):
        media_id = d["media_id"]
        items = d.get("content", {}).get("news_item", [])
        title = items[0].get("title", "(no title)") if items else "(empty)"
        print(f"  [{i+1}] {title!r}")
        print(f"       media_id: {media_id}")

    if not drafts:
        print("(nothing to delete)")
        return

    print(f"\nDeleting {len(drafts)} draft(s)...")
    deleted = 0
    for d in drafts:
        media_id = d["media_id"]
        res = delete_draft(token, media_id)
        errcode = res.get("errcode", 0)
        if errcode == 0:
            print(f"  ✓ deleted {media_id[:24]}...")
            deleted += 1
        else:
            print(f"  ✗ {media_id[:24]}... errcode={errcode} errmsg={res.get('errmsg')}")

    print(f"\n{deleted}/{len(drafts)} deleted.")

    # Verify
    remaining = list_all_drafts(token)
    print(f"\nRemaining drafts: {len(remaining)}")


if __name__ == "__main__":
    main()
