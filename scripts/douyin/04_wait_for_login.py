# /// script
# requires-python = ">=3.10"
# dependencies = ["playwright>=1.49", "python-dotenv>=1.0", "httpx>=0.27"]
# ///
"""Poll the creator-center tab until it lands on the logged-in path.

The login QR is on creator.douyin.com/. Once scanned + confirmed in the
Douyin app, the SPA redirects to /creator-micro/... — we watch the URL
and exit when that happens. Times out after 5 minutes.

Run: uv run scripts/douyin/04_wait_for_login.py
"""

from __future__ import annotations

import time

from playwright.sync_api import sync_playwright

from common import cdp_ws_url, screenshot_path


TIMEOUT_SECS = 300
POLL_INTERVAL = 3


def main() -> None:
    with sync_playwright() as p:
        browser = p.chromium.connect_over_cdp(cdp_ws_url())
        ctx = browser.contexts[0]

        target = None
        for pg in ctx.pages:
            if "creator.douyin.com" in pg.url:
                target = pg
                break
        if target is None:
            raise SystemExit("no creator.douyin.com tab found")

        deadline = time.time() + TIMEOUT_SECS
        last_url = ""
        while time.time() < deadline:
            url = target.url
            if url != last_url:
                print(f"  [{int(TIMEOUT_SECS - (deadline - time.time())):3}s] URL = {url}")
                last_url = url
            if "/creator-micro" in url or "/home" in url and "creator.douyin.com" in url:
                print("\n✓ Logged in (URL contains /creator-micro)")
                out = screenshot_path("04_logged_in.png")
                target.screenshot(path=str(out), full_page=False)
                print(f"  Screenshot: {out}")
                # Pull the user identity if available
                ident = target.evaluate(
                    """
                    () => {
                      // Look for nickname / avatar in common locations
                      const text = document.body.innerText;
                      const m = text.match(/[\\u4e00-\\u9fa5\\w-]{2,20}/);
                      return {
                        title: document.title,
                        url: location.href,
                      };
                    }
                    """
                )
                print(f"  Title: {ident.get('title')}")
                return
            time.sleep(POLL_INTERVAL)

        print(f"\n✗ Timed out after {TIMEOUT_SECS}s without redirect.")
        out = screenshot_path("04_timeout.png")
        target.screenshot(path=str(out), full_page=False)
        print(f"  Final screenshot: {out}")
        raise SystemExit(1)


if __name__ == "__main__":
    main()
