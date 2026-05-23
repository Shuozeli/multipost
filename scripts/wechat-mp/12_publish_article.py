# /// script
# requires-python = ">=3.10"
# dependencies = ["httpx>=0.27", "python-dotenv>=1.0", "markdown>=3.6"]
# ///
"""Convert a markdown file to WeChat-MP-friendly HTML, create a draft, optionally publish.

DEFAULTS TO DRAFT-ONLY. Pass --publish to call freepublish/submit, which
makes the article visible to followers and consumes the daily publish slot.

Usage:
  # Just create the draft
  uv run scripts/wechat-mp/12_publish_article.py articles/2026-05-15-financial-digest.md \\
      --title "财经早报 2026-05-15：AI硬件、中东危局、美债飙升" \\
      --digest "AI替代岗位、深圳硬件造富、理想L9搭载自研5nm芯片、中东危局升级、美债飙升"

  # Create AND publish (irreversible — counts against daily limit)
  uv run scripts/wechat-mp/12_publish_article.py articles/2026-05-15-financial-digest.md \\
      --title "..." --digest "..." --publish
"""

from __future__ import annotations

import argparse
import time
from pathlib import Path

import httpx
import markdown

from common import (
    API_BASE,
    get_stable_token,
    load_artifacts,
    pretty,
    save_artifacts,
)


# Inline-style HTML wrapper for WeChat MP. External stylesheets and many CSS
# classes are stripped by MP, so we ship inline styles.
def wrap_article(html_body: str) -> str:
    style_p = "font-size:16px;line-height:1.8;margin:14px 0;color:#333;"
    style_h2 = ("font-size:19px;line-height:1.5;margin:24px 0 12px;color:#1a1a1a;"
                "border-left:4px solid #c9302c;padding-left:10px;font-weight:600;")
    style_ul = "padding-left:20px;margin:8px 0;"
    style_li = "margin:8px 0;line-height:1.75;"
    style_hr = "border:none;border-top:1px dashed #ccc;margin:24px 0;"
    style_strong = "color:#c9302c;"

    # Cheap inline-styling: replace bare tags with styled equivalents.
    # The markdown lib emits clean HTML5 we can rewrite without parsing.
    return (
        html_body
        .replace("<h2>", f'<h2 style="{style_h2}">')
        .replace("<p>", f'<p style="{style_p}">')
        .replace("<ul>", f'<ul style="{style_ul}">')
        .replace("<li>", f'<li style="{style_li}">')
        .replace("<hr />", f'<hr style="{style_hr}">')
        .replace("<hr>", f'<hr style="{style_hr}">')
        .replace("<strong>", f'<strong style="{style_strong}">')
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("file", type=Path, help="Path to a markdown file")
    parser.add_argument("--title", required=True, help="Article title (<= 64 chars)")
    parser.add_argument("--digest", required=True, help="Short summary (<= 120 chars)")
    parser.add_argument("--author", required=True)
    parser.add_argument("--source-url", default="",
                        help="Optional content_source_url (the '阅读原文' link)")
    parser.add_argument("--publish", action="store_true",
                        help="Call freepublish/submit after draft creation. "
                             "IRREVERSIBLE: visible to followers, counts against daily limit.")
    parser.add_argument("--allow-comments", action="store_true",
                        help="Enable comments (default: disabled)")
    args = parser.parse_args()

    if len(args.title) > 64:
        raise SystemExit(f"title is {len(args.title)} chars; WeChat MP cap is 64.")
    if len(args.digest) > 120:
        raise SystemExit(f"digest is {len(args.digest)} chars; WeChat MP cap is 120.")

    md_source = args.file.read_text(encoding="utf-8")
    raw_html = markdown.markdown(md_source, extensions=["extra"])
    html_body = wrap_article(raw_html)

    print(f"Title:        {args.title!r}  ({len(args.title)} chars)")
    print(f"Digest:       {args.digest!r}  ({len(args.digest)} chars)")
    print(f"Markdown:     {args.file}  ({len(md_source):,} bytes)")
    print(f"HTML body:    {len(html_body):,} bytes")
    print(f"Author:       {args.author}")
    print(f"Comments:     {'enabled' if args.allow_comments else 'disabled'}")
    print(f"Mode:         {'PUBLISH (irreversible)' if args.publish else 'draft only'}")

    artifacts = load_artifacts()
    thumb_id = artifacts.get("permanent_image_media_id")
    if not thumb_id:
        raise SystemExit(
            "No permanent_image_media_id in artifacts.json. "
            "Run 05_upload_permanent_image.py first to set up a cover image."
        )
    print(f"Cover image:  {thumb_id[:24]}...")

    token = get_stable_token()

    # --- Step 1: create draft ---
    article = {
        "article_type": "news",
        "title": args.title,
        "author": args.author,
        "digest": args.digest,
        "content": html_body,
        "content_source_url": args.source_url,
        "thumb_media_id": thumb_id,
        "need_open_comment": 1 if args.allow_comments else 0,
        "only_fans_can_comment": 0,
    }
    body = {"articles": [article]}
    print("\nStep 1: create draft via cgi-bin/draft/add ...")
    resp = httpx.post(
        f"{API_BASE}/cgi-bin/draft/add",
        params={"access_token": token},
        json=body,
        timeout=30.0,
    )
    resp.raise_for_status()
    data = resp.json()
    if data.get("errcode", 0) != 0:
        raise SystemExit(f"draft/add failed: {pretty(data)}")
    draft_media_id = data["media_id"]
    print(f"  ✓ draft_media_id = {draft_media_id}")
    artifacts["latest_draft_media_id"] = draft_media_id
    artifacts["latest_draft_title"] = args.title
    save_artifacts(artifacts)

    if not args.publish:
        print("\n(stopped here — pass --publish to call freepublish/submit)")
        print(f"\nDraft is now visible in the MP admin console at")
        print(f"  https://mp.weixin.qq.com → 草稿箱")
        return

    # --- Step 2: publish ---
    print("\nStep 2: publish via cgi-bin/freepublish/submit ...")
    resp = httpx.post(
        f"{API_BASE}/cgi-bin/freepublish/submit",
        params={"access_token": token},
        json={"media_id": draft_media_id},
        timeout=30.0,
    )
    resp.raise_for_status()
    data = resp.json()
    if data.get("errcode", 0) != 0:
        raise SystemExit(f"freepublish/submit failed: {pretty(data)}")
    publish_id = data.get("publish_id")
    msg_data_id = data.get("msg_data_id")
    print(f"  ✓ submit accepted")
    print(f"    publish_id:  {publish_id}")
    print(f"    msg_data_id: {msg_data_id}")
    artifacts["latest_publish_id"] = publish_id
    artifacts["latest_msg_data_id"] = msg_data_id
    save_artifacts(artifacts)

    # --- Step 3: poll status ---
    print("\nStep 3: poll cgi-bin/freepublish/get for status ...")
    for attempt in range(1, 13):  # ~1 minute
        time.sleep(5)
        resp = httpx.post(
            f"{API_BASE}/cgi-bin/freepublish/get",
            params={"access_token": token},
            json={"publish_id": publish_id},
            timeout=15.0,
        )
        status = resp.json()
        publish_status = status.get("publish_status")
        # 0=success, 1=publishing, 2=fail, 3=audit fail, 4=text-format fail,
        # 5=in-review, 6=admin revoked
        names = {0: "success", 1: "publishing", 2: "fail",
                 3: "audit fail", 4: "format fail",
                 5: "in review", 6: "admin revoked"}
        label = names.get(publish_status, f"unknown({publish_status})")
        print(f"  [{attempt:2}] publish_status={publish_status} ({label})")
        if publish_status == 0:
            items = status.get("article_detail", {}).get("item", [])
            for it in items:
                print(f"    article_url: {it.get('article_url')}")
            return
        if publish_status in (2, 3, 4, 6):
            print(pretty(status))
            raise SystemExit(f"publish failed: {label}")

    print("(still publishing after 1 min — check MP admin manually)")


if __name__ == "__main__":
    main()
