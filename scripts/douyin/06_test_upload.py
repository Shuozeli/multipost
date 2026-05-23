# /// script
# requires-python = ">=3.10"
# dependencies = ["playwright>=1.49", "python-dotenv>=1.0", "httpx>=0.27"]
# ///
"""Upload a small test video to Douyin's creator center to map the
post-upload UI.

DOES NOT CLICK 发布. We:
  1. Navigate to /creator-micro/content/upload
  2. Set the test video on the hidden file input
  3. Wait for the post-upload form to render
  4. Screenshot + dump selectors for title, caption, tags, visibility, Publish

Uses the CC-BY Naztronomy timelapse from the YouTube prototype
(scripts/youtube/downloads/sdRcU5LMMlU.mp4, ~11s, 762KB).

Run: uv run scripts/douyin/06_test_upload.py
"""

from __future__ import annotations

import json
import time
from pathlib import Path

from playwright.sync_api import sync_playwright

from common import SCRIPT_DIR, cdp_ws_url, screenshot_path


VIDEO_PATH = (
    SCRIPT_DIR.parent / "youtube" / "downloads" / "sdRcU5LMMlU.mp4"
)
UPLOAD_URL = "https://creator.douyin.com/creator-micro/content/upload"


def main() -> None:
    if not VIDEO_PATH.exists():
        raise SystemExit(f"test video not found at {VIDEO_PATH}")
    print(f"Video: {VIDEO_PATH}  ({VIDEO_PATH.stat().st_size:,} bytes)")

    with sync_playwright() as p:
        browser = p.chromium.connect_over_cdp(cdp_ws_url())
        ctx = browser.contexts[0]

        target = None
        for pg in ctx.pages:
            if "creator.douyin.com" in pg.url:
                target = pg
                break
        if target is None:
            raise SystemExit("no creator.douyin.com tab open — run 01_check_login.py first")

        print(f"Navigating to {UPLOAD_URL}")
        target.goto(UPLOAD_URL, wait_until="domcontentloaded", timeout=30_000)
        time.sleep(3)

        out0 = screenshot_path("06a_before_upload.png")
        target.screenshot(path=str(out0), full_page=False)
        print(f"  before-upload screenshot: {out0}")

        # Set the file. playwright transfers the bytes over CDP — Chrome on the
        # remote host doesn't need local filesystem access to the path.
        file_input = target.locator('input[type="file"]').first
        print(f"\nSetting file on the hidden input ...")
        file_input.set_input_files(str(VIDEO_PATH))
        print("  ✓ set_input_files dispatched")

        # Wait for the post-upload form. Best signal: the URL changes to
        # /creator-micro/content/post/video or a title input appears.
        print("\nWaiting for the post-upload form to render (up to 90s) ...")
        deadline = time.time() + 90
        last_url = target.url
        while time.time() < deadline:
            url = target.url
            if url != last_url:
                print(f"  URL → {url}")
                last_url = url
            # Look for any visible text input or contenteditable that's
            # appeared since the upload started.
            n = target.evaluate(
                """
                () => {
                  let n = 0;
                  for (const el of document.querySelectorAll('input[type="text"], textarea, [contenteditable="true"]')) {
                    const r = el.getBoundingClientRect();
                    if (r.width > 50 && r.height > 10) n++;
                  }
                  return n;
                }
                """
            )
            if n > 0 or "post" in url or "video" in url:
                print(f"  ✓ post-upload UI ready (visible text-inputs={n}, url={url})")
                break
            time.sleep(2)

        time.sleep(3)
        out1 = screenshot_path("06b_after_upload.png")
        target.screenshot(path=str(out1), full_page=False)
        print(f"\nafter-upload screenshot: {out1}")

        # Probe the form.
        print("\nProbing post-upload form ...")
        probe = target.evaluate(
            """
            () => {
              const sumEl = (el) => {
                if (!el) return null;
                const r = el.getBoundingClientRect();
                return {
                  tag: el.tagName.toLowerCase(),
                  type: el.type || null,
                  placeholder: el.placeholder || null,
                  aria_label: el.getAttribute('aria-label') || null,
                  contenteditable: el.getAttribute('contenteditable') || null,
                  cls: (el.className || '').toString().slice(0, 100),
                  text: (el.innerText || '').trim().slice(0, 80),
                  x: Math.round(r.x), y: Math.round(r.y),
                  w: Math.round(r.width), h: Math.round(r.height),
                };
              };
              const inputs = Array.from(
                document.querySelectorAll('input[type="text"], textarea, [contenteditable="true"]')
              ).map(sumEl).filter(e => e && e.w > 50 && e.h > 10);

              const wanted = ['发布', '存草稿', '保存草稿', '取消', '上一步', '定时发布'];
              const btns = [];
              for (const el of document.querySelectorAll('button, [role="button"], div, span')) {
                const text = (el.innerText || '').trim();
                if (wanted.some(w => text === w || text.startsWith(w))) {
                  const r = el.getBoundingClientRect();
                  if (r.width >= 40 && r.height >= 20 && r.width <= 300) {
                    btns.push(sumEl(el));
                  }
                }
              }

              // Look for visibility / privacy controls
              const visBits = [];
              for (const el of document.querySelectorAll('label, span, div')) {
                const t = (el.innerText || '').trim();
                if (/公开|好友|私密|仅自己|隐私|可见/.test(t) && t.length < 30) {
                  visBits.push(sumEl(el));
                }
              }

              return {
                inputs: inputs.slice(0, 12),
                buttons: btns.slice(0, 15),
                visibility_bits: visBits.slice(0, 15),
              };
            }
            """
        )

        print(f"\n  Visible text inputs ({len(probe['inputs'])}):")
        for it in probe["inputs"]:
            print(f"    {it['tag']:8} placeholder={it['placeholder']!r:30} contenteditable={it['contenteditable']!r:8} @ ({it['x']},{it['y']}) {it['w']}x{it['h']}  cls={it['cls']!r}")

        print(f"\n  Action buttons ({len(probe['buttons'])}):")
        for b in probe["buttons"]:
            print(f"    {b['tag']:6} {b['text']!r:18} @ ({b['x']},{b['y']}) {b['w']}x{b['h']}  cls={b['cls']!r}")

        print(f"\n  Visibility-related labels ({len(probe['visibility_bits'])}):")
        for v in probe["visibility_bits"][:10]:
            print(f"    {v['tag']:6} {v['text']!r:18} @ ({v['x']},{v['y']})  cls={v['cls']!r}")

        print("\nDONE (intentionally not clicking 发布)")


if __name__ == "__main__":
    main()
