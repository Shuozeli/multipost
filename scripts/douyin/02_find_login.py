# /// script
# requires-python = ">=3.10"
# dependencies = ["playwright>=1.49", "python-dotenv>=1.0", "httpx>=0.27"]
# ///
"""Find Douyin's login entry point and capture the QR-code page.

01_check_login showed we land on the marketing splash (`抖音创作者中心·创作者`),
not the logged-in dashboard. This script:

  1. Reuses the existing tab from 01.
  2. Looks for "登录" (login) links / buttons on the splash.
  3. Clicks the most likely one, or falls back to the canonical login URL.
  4. Screenshots whatever we land on (typically the QR code page).
  5. Dumps the selectors of any interactive elements so we know what
     to target in the Rust publisher.

Run: uv run scripts/douyin/02_find_login.py
"""

from __future__ import annotations

import json
import time

from playwright.sync_api import sync_playwright

from common import cdp_ws_url, screenshot_path


CREATOR_URL = "https://creator.douyin.com"


def main() -> None:
    ws = cdp_ws_url()
    print(f"Connecting via {ws}")

    with sync_playwright() as p:
        browser = p.chromium.connect_over_cdp(ws)
        ctx = browser.contexts[0]

        # Find or open a creator.douyin.com page.
        target = None
        for pg in ctx.pages:
            if "creator.douyin.com" in pg.url:
                target = pg
                break
        if target is None:
            target = ctx.new_page()
            target.goto(CREATOR_URL, wait_until="domcontentloaded", timeout=30_000)
        else:
            # Make sure we're on the marketing splash, not somewhere else.
            target.goto(CREATOR_URL, wait_until="domcontentloaded", timeout=30_000)
        time.sleep(2)

        out_landing = screenshot_path("02a_landing_top.png")
        target.screenshot(path=str(out_landing), full_page=False)
        print(f"  landing screenshot: {out_landing}")

        # Scan all clickable elements for login-y text.
        print("\nProbing for login candidates ...")
        candidates = target.evaluate(
            """
            () => {
              const wanted = ['登录', '登 录', 'Login', 'Sign in', '立即登录'];
              const hits = [];
              const els = document.querySelectorAll('a, button, [role="button"], [class*="login"], [class*="Login"]');
              for (const el of els) {
                const text = (el.innerText || el.textContent || '').trim();
                const aria = el.getAttribute('aria-label') || '';
                const cls = el.className || '';
                if (wanted.some(w => text.includes(w) || aria.includes(w)) ||
                    /login/i.test(cls)) {
                  const r = el.getBoundingClientRect();
                  if (r.width > 0 && r.height > 0) {
                    hits.push({
                      tag: el.tagName.toLowerCase(),
                      text: text.slice(0, 80),
                      href: el.getAttribute('href') || null,
                      cls: (el.className || '').slice(0, 80),
                      x: Math.round(r.x), y: Math.round(r.y),
                      w: Math.round(r.width), h: Math.round(r.height),
                    });
                  }
                }
              }
              return hits.slice(0, 20);
            }
            """
        )
        print(f"  {len(candidates)} candidate(s):")
        for c in candidates:
            print(f"    {c['tag']:8} {c['text']!r:30}  @ ({c['x']},{c['y']}) {c['w']}x{c['h']}  href={c['href']}  cls={c['cls']!r}")

        # Try to click the first link-style candidate (has href and "登录" text).
        target_url: str | None = None
        for c in candidates:
            if c["href"] and ("登录" in c["text"] or "Login" in c["text"]):
                target_url = c["href"]
                break

        if target_url:
            # Resolve relative URLs.
            if target_url.startswith("/"):
                target_url = "https://creator.douyin.com" + target_url
            print(f"\nNavigating to login URL: {target_url}")
            target.goto(target_url, wait_until="domcontentloaded", timeout=30_000)
        else:
            print("\nNo link-style candidate found; falling back to clicking a button.")
            # Try clicking the first button-style candidate
            for c in candidates:
                if c["tag"] in ("button", "div", "span") and ("登录" in c["text"] or "Login" in c["text"]):
                    try:
                        target.mouse.click(c["x"] + c["w"] // 2, c["y"] + c["h"] // 2)
                        print(f"  clicked at ({c['x']+c['w']//2}, {c['y']+c['h']//2}) {c['text']!r}")
                        break
                    except Exception as e:
                        print(f"  click failed: {e}")

        time.sleep(4)  # let the login flow settle

        url_after = target.url
        title_after = target.title()
        print(f"\nAfter login click:")
        print(f"  URL:   {url_after}")
        print(f"  Title: {title_after}")

        out_login = screenshot_path("02b_login_page.png")
        target.screenshot(path=str(out_login), full_page=False)
        print(f"  login-page screenshot: {out_login}")

        # Dump the URL params + check for QR code
        host = target.evaluate("() => location.host")
        path = target.evaluate("() => location.pathname")
        # Look for img elements (QR codes are usually rendered as <img> or <canvas>)
        qr_candidates = target.evaluate(
            """
            () => {
              const out = [];
              for (const img of document.querySelectorAll('img, canvas')) {
                const r = img.getBoundingClientRect();
                if (r.width >= 100 && r.height >= 100 && Math.abs(r.width - r.height) < 30) {
                  out.push({
                    tag: img.tagName.toLowerCase(),
                    src: img.getAttribute('src') || null,
                    x: Math.round(r.x), y: Math.round(r.y),
                    w: Math.round(r.width), h: Math.round(r.height),
                  });
                }
              }
              return out;
            }
            """
        )
        print(f"\n  Host: {host}  Path: {path}")
        print(f"  Square image/canvas candidates (likely QR codes): {len(qr_candidates)}")
        for q in qr_candidates[:5]:
            src = (q["src"] or "")[:80]
            print(f"    {q['tag']:6} {q['w']}x{q['h']} @ ({q['x']},{q['y']})  src={src!r}")


if __name__ == "__main__":
    main()
