# /// script
# requires-python = ">=3.10"
# dependencies = ["httpx>=0.27", "python-dotenv>=1.0", "markdown>=3.6"]
# ///
"""Create a multi-article WeChat-MP draft (图文消息组) in one call.

Unlike `12_publish_article.py` which always sends `articles: [single]`,
this script reads N input files and packs them into a single
`cgi-bin/draft/add` body. The result is one draft entry in 草稿箱
containing all N articles as a group post.

Each input is `<file.md>::<title>::<digest>` — three colon-separated
parts. Title and digest are required because WeChat won't accept empty
strings on those fields. Digest is capped at 120 chars (WeChat's hard
limit) and the script will fail loudly if any exceeds it.

Usage:
  uv run scripts/wechat-mp/14_publish_batch.py \\
    "articles/2026-05-20-financial-digest.md::财经早报 2026年5月20日::每日财经摘要" \\
    "articles/2026-05-20-nvidia-ecosystem.md::英伟达合作生态::AI 基础设施盟主成型" \\
    ...

This script is DRAFT-ONLY by design. WeChat's `freepublish/submit` is
gated 48001 for individual subscription accounts anyway, so we never
expose --publish here.
"""

from __future__ import annotations

import argparse
from pathlib import Path

import httpx
import markdown

from common import API_BASE, get_stable_token, load_artifacts, pretty


def wrap_article(html_body: str) -> str:
    """Same inline-styling pass as 12_publish_article.py. Duplicated
    intentionally to keep this script standalone."""
    style_p = "font-size:16px;line-height:1.8;margin:14px 0;color:#333;"
    style_h2 = (
        "font-size:19px;line-height:1.5;margin:24px 0 12px;color:#1a1a1a;"
        "border-left:4px solid #c9302c;padding-left:10px;font-weight:600;"
    )
    style_h3 = (
        "font-size:17px;line-height:1.5;margin:18px 0 10px;color:#333;font-weight:600;"
    )
    style_ul = "padding-left:20px;margin:8px 0;"
    style_li = "margin:8px 0;line-height:1.75;"
    style_hr = "border:none;border-top:1px dashed #ccc;margin:24px 0;"
    style_strong = "color:#c9302c;"
    style_blockquote = (
        "border-left:3px solid #c9302c;background:#fdf3f3;color:#555;"
        "padding:8px 14px;margin:14px 0;font-size:15px;"
    )

    return (
        html_body
        .replace("<p>", f'<p style="{style_p}">')
        .replace("<h2>", f'<h2 style="{style_h2}">')
        .replace("<h3>", f'<h3 style="{style_h3}">')
        .replace("<ul>", f'<ul style="{style_ul}">')
        .replace("<ol>", f'<ol style="{style_ul}">')
        .replace("<li>", f'<li style="{style_li}">')
        .replace("<hr />", f'<hr style="{style_hr}" />')
        .replace("<hr/>", f'<hr style="{style_hr}" />')
        .replace("<hr>", f'<hr style="{style_hr}" />')
        .replace("<strong>", f'<strong style="{style_strong}">')
        .replace("<blockquote>", f'<blockquote style="{style_blockquote}">')
    )


def render_article(md_file: Path, title: str, digest: str, thumb_id: str, author: str) -> dict:
    if len(digest) > 120:
        raise SystemExit(
            f"digest for {md_file} is {len(digest)} chars; WeChat caps at 120"
        )
    md_source = md_file.read_text(encoding="utf-8")
    raw_html = markdown.markdown(md_source, extensions=["extra"])
    html = wrap_article(raw_html)
    return {
        "article_type": "news",
        "title": title,
        "author": author,
        "digest": digest,
        "content": html,
        "content_source_url": "",
        "thumb_media_id": thumb_id,
        "need_open_comment": 0,
        "only_fans_can_comment": 0,
    }


def parse_arg(s: str) -> tuple[Path, str, str]:
    parts = s.split("::")
    if len(parts) != 3:
        raise SystemExit(
            f"expected 3 colon-separated parts (path::title::digest), got: {s!r}"
        )
    p = (Path(__file__).parent / parts[0]).resolve()
    if not p.exists():
        raise SystemExit(f"file not found: {p}")
    return p, parts[1].strip(), parts[2].strip()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "items",
        nargs="+",
        help='One or more "<file.md>::<title>::<digest>" specs.',
    )
    parser.add_argument("--author", required=True)
    args = parser.parse_args()

    artifacts = load_artifacts()
    thumb_id = artifacts.get("permanent_image_media_id")
    if not thumb_id:
        raise SystemExit(
            "No permanent_image_media_id in artifacts.json — run "
            "05_upload_permanent_image.py first."
        )

    parsed = [parse_arg(s) for s in args.items]
    print(f"Building batch draft with {len(parsed)} article(s):")
    for p, t, d in parsed:
        print(f"  - {p.name}  title={t!r}  digest={len(d)} chars")

    articles = [
        render_article(p, t, d, thumb_id, args.author) for (p, t, d) in parsed
    ]

    token = get_stable_token()
    print("\nPOST cgi-bin/draft/add ...")
    resp = httpx.post(
        f"{API_BASE}/cgi-bin/draft/add",
        params={"access_token": token},
        json={"articles": articles},
        timeout=30.0,
    )
    resp.raise_for_status()
    data = resp.json()
    if data.get("errcode", 0) != 0:
        raise SystemExit(f"draft/add failed: {pretty(data)}")
    draft_media_id = data["media_id"]
    print(f"\n✓ batch draft created")
    print(f"  draft_media_id: {draft_media_id}")
    print(f"  articles:       {len(articles)}")
    print()
    print("Open https://mp.weixin.qq.com → 草稿箱 — the batch will appear as one")
    print("entry containing all articles; click in to edit each one.")


if __name__ == "__main__":
    main()
