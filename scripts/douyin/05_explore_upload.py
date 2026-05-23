# /// script
# requires-python = ">=3.10"
# dependencies = ["playwright>=1.49", "python-dotenv>=1.0", "httpx>=0.27"]
# ///
"""Map the Douyin video-upload UI on creator.douyin.com.

Starting from the logged-in dashboard, we click the "发布视频" (Post video)
card and snapshot the upload page so we know what selectors the Rust
publisher will need:

  - file input for the video
  - title / caption textarea
  - tag / hashtag input
  - visibility toggle (public / friends / private)
  - "发布" (Publish) submit button

Read-only: doesn't upload anything.

Run: uv run scripts/douyin/05_explore_upload.py
"""

from __future__ import annotations

import json
import time

from playwright.sync_api import sync_playwright

from common import cdp_ws_url, screenshot_path


# Direct URL for the new-video page; we'll fall back to clicking the dashboard
# card if this redirects.
UPLOAD_URLS = [
    "https://creator.douyin.com/creator-micro/content/upload",
    "https://creator.douyin.com/creator-micro/content/post/video",
]


def main() -> None:
    with sync_playwright() as p:
        browser = p.chromium.connect_over_cdp(cdp_ws_url())
        ctx = browser.contexts[0]

        # Find an existing creator.douyin.com tab.
        target = None
        for pg in ctx.pages:
            if "creator.douyin.com" in pg.url:
                target = pg
                break
        if target is None:
            raise SystemExit("no creator.douyin.com tab — run 01_check_login.py first")

        # Try direct URLs first.
        landed = None
        for url in UPLOAD_URLS:
            print(f"Navigating to {url}")
            target.goto(url, wait_until="domcontentloaded", timeout=30_000)
            time.sleep(3)
            current = target.url
            print(f"  → {current}")
            if "upload" in current.lower() or "post" in current.lower() or "video" in current.lower():
                landed = current
                break
        if landed is None:
            print("\nDirect URLs didn't land us on upload — falling back to clicking the dashboard card.")
            target.goto("https://creator.douyin.com/creator-micro/home", wait_until="domcontentloaded", timeout=30_000)
            time.sleep(3)
            # Click the "发布视频" card (look for text "发布视频" with role=link/button)
            target.evaluate(
                """
                () => {
                  for (const el of document.querySelectorAll('a, button, div, span')) {
                    const t = (el.innerText || '').trim();
                    if (t === '发布视频' || t.startsWith('发布视频\\n')) {
                      const r = el.getBoundingClientRect();
                      if (r.width > 0 && r.height > 0) {
                        el.click();
                        return {x: r.x, y: r.y, w: r.width, h: r.height, text: t};
                      }
                    }
                  }
                  return null;
                }
                """
            )
            time.sleep(4)
            print(f"  after click: {target.url}")
            landed = target.url

        out = screenshot_path("05_upload_page.png")
        target.screenshot(path=str(out), full_page=False)
        print(f"\nScreenshot: {out}")
        print(f"Final URL: {target.url}")
        print(f"Title:     {target.title()}")

        # Probe the upload UI for key elements.
        print("\nProbing upload UI elements ...")
        probe = target.evaluate(
            """
            () => {
              const out = {};
              const sumEl = (el) => {
                if (!el) return null;
                const r = el.getBoundingClientRect();
                return {
                  tag: el.tagName.toLowerCase(),
                  type: el.type || null,
                  id: el.id || null,
                  name: el.name || null,
                  placeholder: el.placeholder || null,
                  aria_label: el.getAttribute('aria-label') || null,
                  cls: (el.className || '').toString().slice(0, 120),
                  text: (el.innerText || '').trim().slice(0, 80),
                  x: Math.round(r.x), y: Math.round(r.y),
                  w: Math.round(r.width), h: Math.round(r.height),
                  visible: r.width > 0 && r.height > 0,
                };
              };

              // File inputs (always interesting — drag-drop areas have hidden ones)
              out.file_inputs = Array.from(document.querySelectorAll('input[type="file"]')).map(sumEl);

              // Visible text inputs / textareas
              out.text_inputs = Array.from(
                document.querySelectorAll('input[type="text"], textarea, [contenteditable="true"]')
              ).map(sumEl).filter(e => e && e.visible);

              // Buttons with publish-y text
              const wanted = ['发布', '发表', '保存草稿', 'Publish', '上传', '提交'];
              const btns = [];
              for (const el of document.querySelectorAll('button, a, [role="button"], div')) {
                const text = (el.innerText || '').trim();
                if (wanted.some(w => text === w) || /publish|submit|upload/i.test(el.className || '')) {
                  const r = el.getBoundingClientRect();
                  if (r.width > 0 && r.height > 0) {
                    btns.push(sumEl(el));
                  }
                }
              }
              out.action_buttons = btns.slice(0, 20);

              // Drag-drop zones
              const zones = [];
              for (const el of document.querySelectorAll('[class*="drag"], [class*="drop"], [class*="upload"]')) {
                const r = el.getBoundingClientRect();
                if (r.width >= 200 && r.height >= 100) {
                  zones.push(sumEl(el));
                }
              }
              out.drop_zones = zones.slice(0, 10);

              return out;
            }
            """
        )

        print("\nFile inputs:")
        for fi in probe["file_inputs"][:5]:
            print(f"  {json.dumps(fi)}")

        print(f"\nVisible text inputs/textareas ({len(probe['text_inputs'])}):")
        for ti in probe["text_inputs"][:8]:
            print(f"  {ti['tag']:6} placeholder={ti['placeholder']!r:30} aria={ti['aria_label']!r:20} @ ({ti['x']},{ti['y']}) {ti['w']}x{ti['h']}")

        print(f"\nAction buttons ({len(probe['action_buttons'])}):")
        for b in probe["action_buttons"][:10]:
            print(f"  {b['tag']:8} {b['text']!r:18} @ ({b['x']},{b['y']}) {b['w']}x{b['h']}  cls={b['cls']!r}")

        print(f"\nDrop zones ({len(probe['drop_zones'])}):")
        for z in probe["drop_zones"][:5]:
            print(f"  {z['tag']:6} {z['w']}x{z['h']} @ ({z['x']},{z['y']})  cls={z['cls']!r}")


if __name__ == "__main__":
    main()
