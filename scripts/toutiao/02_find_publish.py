# /// script
# requires-python = ">=3.10"
# dependencies = ["playwright>=1.49", "python-dotenv>=1.0", "httpx>=0.27"]
# ///
"""Find Toutiao's publish entry points.

Toutiao supports several content types under 创作:
  - 微头条 (micro-headline)       — short text + optional images
  - 头条文章 (article)             — long-form rich text
  - 头条视频 (video)               — uploaded videos
  - 图集 (image set)              — image gallery
  - 问答 (Q&A)

This script scans the dashboard sidebar for these entries and dumps their
URLs / coordinates / labels so we know what to call from the Rust publisher.

Run: uv run scripts/toutiao/02_find_publish.py
"""

from __future__ import annotations

import json

from playwright.sync_api import sync_playwright

from common import cdp_ws_url, screenshot_path


def main() -> None:
    ws = cdp_ws_url()
    print(f"Connecting via {ws}")

    with sync_playwright() as p:
        browser = p.chromium.connect_over_cdp(ws)
        ctx = browser.contexts[0]

        target = None
        for pg in ctx.pages:
            if "mp.toutiao.com" in pg.url:
                target = pg
                break
        if target is None:
            raise SystemExit("no mp.toutiao.com tab — run 01_check_login.py first")

        out_a = screenshot_path("02a_dashboard.png")
        target.screenshot(path=str(out_a), full_page=False)
        print(f"  dashboard screenshot: {out_a}")

        # Sidebar-link discovery. Toutiao uses an SPA so links may be <a> tags
        # with hash-style routes OR <div> elements that click into a route.
        print("\nScanning sidebar / nav for publish entries ...")
        candidates = target.evaluate(
            """
            () => {
              const wanted = [
                '微头条', '头条文章', '头条视频', '图集', '问答',
                '发微头条', '发文章', '发视频', '发图集', '发布', '创作'
              ];
              const out = [];
              const elements = document.querySelectorAll('a, [role="link"], button, [role="button"], li, div, span');
              for (const el of elements) {
                const text = (el.innerText || '').trim();
                // exact-match labels (avoid catching whole-page text)
                if (wanted.some(w => text === w || text === w + '\\u200B')) {
                  const r = el.getBoundingClientRect();
                  if (r.width > 0 && r.height > 0 && r.width < 400) {
                    out.push({
                      tag: el.tagName.toLowerCase(),
                      text,
                      href: el.getAttribute('href') || null,
                      cls: (el.className || '').toString().slice(0, 100),
                      x: Math.round(r.x), y: Math.round(r.y),
                      w: Math.round(r.width), h: Math.round(r.height),
                    });
                  }
                }
              }
              // Dedupe by (text, x, y) preferring elements with href
              const seen = new Map();
              for (const c of out) {
                const key = c.text + ':' + c.x + ':' + c.y;
                if (!seen.has(key) || c.href) seen.set(key, c);
              }
              return Array.from(seen.values()).slice(0, 30);
            }
            """
        )
        for c in candidates:
            print(
                f"  {c['tag']:6} {c['text']:>10}  @ ({c['x']},{c['y']}) {c['w']}x{c['h']}  "
                f"href={c['href']!r}  cls={c['cls']!r}"
            )

        # Try direct URLs known from Toutiao's frontend routing:
        known_urls = [
            ("微头条", "https://mp.toutiao.com/profile_v4/weitoutiao/publish-content/"),
            ("文章", "https://mp.toutiao.com/profile_v4/graphic/publish?source=mp_creation_main"),
            ("视频", "https://mp.toutiao.com/profile_v4/upload-video"),
        ]
        print(f"\nProbing known publish URLs ...")
        for label, url in known_urls:
            target.goto(url, wait_until="domcontentloaded", timeout=20_000)
            import time
            time.sleep(2)
            final = target.url
            title = target.title()
            print(f"  [{label}] → {final}  title={title!r}")
            out = screenshot_path(f"02b_publish_{label}.png")
            target.screenshot(path=str(out), full_page=False)


if __name__ == "__main__":
    main()
