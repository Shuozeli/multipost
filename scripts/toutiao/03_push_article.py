# /// script
# requires-python = ">=3.10"
# dependencies = ["playwright>=1.49", "python-dotenv>=1.0", "httpx>=0.27"]
# ///
"""Push an article to Toutiao's `发布文章` editor (mp.toutiao.com).

Reads the markdown source, strips it to plain text (preserving section
headers + paragraphs), then fills the title input and rich-text body
editor on creator's `graphic/publish` page.

Does NOT click "预览并发布". Toutiao auto-saves to draft as you type;
the result will show up in `草稿箱`.

Run: uv run scripts/toutiao/03_push_article.py
"""

from __future__ import annotations

import argparse
import re
import time
from pathlib import Path

from playwright.sync_api import sync_playwright

from common import SCRIPT_DIR, cdp_ws_url, screenshot_path


PUBLISH_URL = "https://mp.toutiao.com/profile_v4/graphic/publish?source=mp_creation_main"

DEFAULT_ARTICLE = (
    SCRIPT_DIR.parent / "wechat-mp" / "articles" / "2026-05-16-financial-digest.md"
)


def strip_markdown(md: str) -> tuple[str, str]:
    """Return (title, body) extracted from the markdown.

    Title = first `# ` line (if present).
    Body  = the rest, with:
            - `## ` and `### ` headers turned into plain lines (kept as section
              headers; the user can re-format inside Toutiao if desired)
            - `**bold**` → bold-stripped (just inner text)
            - blank-line paragraph breaks preserved
    """
    lines = md.splitlines()
    title = ""
    body_lines: list[str] = []
    for ln in lines:
        if not title and ln.startswith("# "):
            title = ln[2:].strip()
            continue
        # Section headers become plain header-style lines.
        if ln.startswith("### "):
            body_lines.append(ln[4:].strip())
            continue
        if ln.startswith("## "):
            body_lines.append(ln[3:].strip())
            continue
        # Strip **bold** markers but keep the inner text.
        ln = re.sub(r"\*\*(.+?)\*\*", r"\1", ln)
        body_lines.append(ln)
    body = "\n".join(body_lines).strip()
    return title, body


def fill_text_input(page, selector: str, text: str) -> None:
    """Focus an input and replace its value via real typing."""
    elem = page.locator(selector).first
    elem.click()
    elem.fill("")  # clear any existing
    elem.type(text, delay=10)


def fill_contenteditable(page, selector: str, text: str) -> None:
    """Insert text into a rich-text contenteditable element.

    Uses `execCommand('insertText')` because Lexical/Draft-style editors
    listen for InputEvent rather than DOM mutations (same trick that worked
    for Twitter's composer and WxGzh's draft body).
    """
    # First, focus
    page.evaluate(
        """
        (selector) => {
          const el = document.querySelector(selector);
          if (!el) throw new Error('no element matches ' + selector);
          el.focus();
        }
        """,
        selector,
    )
    # Insert in chunks so very long bodies don't time out the eval call.
    CHUNK = 800
    for i in range(0, len(text), CHUNK):
        chunk = text[i : i + CHUNK]
        page.evaluate(
            """
            (chunk) => {
              document.execCommand('insertText', false, chunk);
            }
            """,
            chunk,
        )
        time.sleep(0.05)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--file", type=Path, default=DEFAULT_ARTICLE,
                        help="Markdown source")
    parser.add_argument("--title", default="财经早报 | 2026年5月17日",
                        help="Override the title")
    args = parser.parse_args()

    md = args.file.read_text(encoding="utf-8")
    _, body = strip_markdown(md)
    title = args.title
    print(f"Title:    {title!r}  ({len(title)} chars; Toutiao cap 30)")
    print(f"Body:     {len(body):,} chars from {args.file}")
    if len(title) > 30:
        raise SystemExit(f"title exceeds 30 chars")

    with sync_playwright() as p:
        browser = p.chromium.connect_over_cdp(cdp_ws_url())
        ctx = browser.contexts[0]

        # Reuse the existing mp.toutiao.com tab so we keep the auth session.
        target = None
        for pg in ctx.pages:
            if "mp.toutiao.com" in pg.url:
                target = pg
                break
        if target is None:
            target = ctx.new_page()

        print(f"\nNavigating to {PUBLISH_URL}")
        target.goto(PUBLISH_URL, wait_until="domcontentloaded", timeout=30_000)
        time.sleep(4)  # let the editor mount

        # Dismiss any onboarding tooltip first.
        dismissed = target.evaluate(
            """
            () => {
              for (const el of document.querySelectorAll('button, span, div')) {
                const t = (el.innerText || '').trim();
                if (t === '我知道了' || t === '知道了') {
                  const r = el.getBoundingClientRect();
                  if (r.width > 0 && r.height > 0) { el.click(); return t; }
                }
              }
              return null;
            }
            """
        )
        if dismissed:
            print(f"  dismissed tooltip: {dismissed!r}")

        # Probe the title input and the body editor selectors.
        sel_probe = target.evaluate(
            """
            () => {
              const out = {title_sel: null, body_sel: null};
              for (const el of document.querySelectorAll('input[type="text"], textarea')) {
                const ph = (el.getAttribute('placeholder') || '');
                if (ph.includes('标题') || ph.includes('文章标题')) {
                  out.title_sel = el.tagName.toLowerCase() + (el.className ? '.' + el.className.split(' ').filter(c => c).slice(0,2).join('.') : '');
                  out.title_el_text = ph;
                  break;
                }
              }
              for (const el of document.querySelectorAll('[contenteditable="true"]')) {
                const r = el.getBoundingClientRect();
                if (r.width > 400 && r.height > 100) {
                  // grab a class signature so we can target it later
                  out.body_sel = '[contenteditable="true"]';
                  out.body_classes = (el.className || '').toString().slice(0, 200);
                  out.body_box = {x: r.x, y: r.y, w: r.width, h: r.height};
                  break;
                }
              }
              return out;
            }
            """
        )
        print(f"\n  Selectors detected: {sel_probe}")

        if not sel_probe.get("title_sel"):
            raise SystemExit("could not locate the title input on the publish page")
        if not sel_probe.get("body_sel"):
            raise SystemExit("could not locate the body contenteditable")

        # ---- fill title ----
        print("\nFilling title ...")
        # The first input with placeholder matching '文章标题' is the title.
        title_input_locator = target.locator("input").filter(has_text="").first
        # Safer: use placeholder selector
        title_input = target.get_by_placeholder(re.compile(r"标题"))
        title_input.first.click()
        title_input.first.fill("")
        title_input.first.type(title, delay=8)
        time.sleep(1)

        # ---- fill body ----
        print("Filling body (chunked execCommand insertText) ...")
        fill_contenteditable(target, '[contenteditable="true"]', body)
        time.sleep(2)

        # Verify what landed
        verify = target.evaluate(
            """
            () => {
              const title = (() => {
                for (const el of document.querySelectorAll('input[type="text"], textarea')) {
                  const ph = el.getAttribute('placeholder') || '';
                  if (ph.includes('标题')) return el.value;
                }
                return null;
              })();
              const body = (() => {
                for (const el of document.querySelectorAll('[contenteditable="true"]')) {
                  const r = el.getBoundingClientRect();
                  if (r.width > 400 && r.height > 100) {
                    return (el.innerText || '').slice(0, 600);
                  }
                }
                return null;
              })();
              return {title, body_preview: body};
            }
            """
        )
        print(f"\n  ↳ title in DOM: {verify['title']!r}")
        bp = (verify["body_preview"] or "").replace("\n", " | ")
        print(f"  ↳ body preview: {bp[:300]!r}")

        out = screenshot_path("03_article_filled.png")
        target.screenshot(path=str(out), full_page=False)
        print(f"\n  Screenshot: {out}")
        print("\nDONE. Toutiao auto-saves to draft as you type — find it in 草稿箱.")
        print("NOT clicked: 预览并发布 (would publish to followers).")


if __name__ == "__main__":
    main()
