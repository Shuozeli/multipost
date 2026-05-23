# /// script
# requires-python = ">=3.10"
# dependencies = ["httpx>=0.27", "python-dotenv>=1.0", "Pillow>=10.0"]
# ///
"""Create a draft article (private — visible only in the MP admin console).

A draft is NOT public. Followers do not see it. It only becomes public
when 07_publish_draft.py is run against it.

Requires a permanent image already in the material library — use 05 first.

Endpoint: POST cgi-bin/draft/add
Body:     {"articles": [{title, author, content (HTML), thumb_media_id, ...}]}
Returns:  {"media_id": "..."}   ← this is the DRAFT media_id, used by freepublish

Stores the draft media_id in artifacts.json.

Run: uv run scripts/wechat-mp/06_create_draft.py
"""

from __future__ import annotations

import httpx

from common import (
    API_BASE,
    get_stable_token,
    load_artifacts,
    pretty,
    save_artifacts,
)


def main() -> None:
    artifacts = load_artifacts()
    thumb_media_id = artifacts.get("permanent_image_media_id")
    if not thumb_media_id:
        raise SystemExit(
            "permanent_image_media_id not in artifacts.json. "
            "Run 05_upload_permanent_image.py first."
        )

    token = get_stable_token()

    article = {
        "article_type": "news",
        "title": "[测试] multipost 集成测试草稿",
        "author": "multipost",
        # WeChat caps digest at 120 chars (errcode 45004 if exceeded)
        "digest": "multipost test draft — private, not published",
        "content": (
            "<h2>multipost · WeChat MP integration test</h2>"
            "<p>This is an automatically-generated draft used to validate the "
            "<code>cgi-bin/draft/add</code> endpoint. It is private — followers "
            "will not see this unless <code>cgi-bin/freepublish/submit</code> "
            "is explicitly called against it.</p>"
            "<p>Generated at draft creation time by the test harness in "
            "<code>shuozeli/_wip/multipost/scripts/wechat-mp/</code>.</p>"
        ),
        "content_source_url": "",
        "thumb_media_id": thumb_media_id,
        "need_open_comment": 0,
        "only_fans_can_comment": 0,
    }
    body = {"articles": [article]}

    resp = httpx.post(
        f"{API_BASE}/cgi-bin/draft/add",
        params={"access_token": token},
        json=body,
        timeout=30.0,
    )
    resp.raise_for_status()
    data = resp.json()
    print(pretty(data))

    if "errcode" in data and data["errcode"] != 0:
        raise SystemExit(f"WeChat error: {data}")

    artifacts["draft_media_id"] = data["media_id"]
    save_artifacts(artifacts)
    print(f"\n✓ draft created. draft media_id = {data['media_id']}")
    print(f"  → ready to publish via 07_publish_draft.py (NOT WRITTEN YET — will ask first)")


if __name__ == "__main__":
    main()
