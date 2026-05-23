# /// script
# requires-python = ">=3.10"
# dependencies = ["playwright>=1.49", "python-dotenv>=1.0", "httpx>=0.27"]
# ///
"""Open Douyin creator center on the targeted Chrome profile and report state.

The target Chrome runs on a host you control (typically reached over the
local network or via an SSH tunnel). Set `CDP_URL` in `scripts/douyin/.env`
to the HTTP CDP endpoint, e.g.:

    CDP_URL=http://chrome-host:9302

We attach via CDP, reuse the existing persistent context (the user-data-dir's
cookies, etc.), navigate to creator.douyin.com, screenshot the result, and
classify the page as logged-in / login-wall / ambiguous.

Run: uv run scripts/douyin/01_check_login.py
"""

from __future__ import annotations

import time

from playwright.sync_api import sync_playwright

from common import cdp_ws_url, screenshot_path


CREATOR_URL = "https://creator.douyin.com"


def main() -> None:
    ws = cdp_ws_url()
    print(f"Connecting to CDP over WS: {ws}")

    with sync_playwright() as p:
        browser = p.chromium.connect_over_cdp(ws)
        print(f"  ✓ Chrome/{browser.version}")
        print(f"  Existing contexts: {len(browser.contexts)}")

        # Use the existing (persistent) context — that's where the
        # profile's cookies live. Connecting to remote Chrome via CDP
        # generally exposes one context per --user-data-dir.
        ctx = browser.contexts[0]
        existing_pages = [pg for pg in ctx.pages if pg.url != "about:blank"]
        print(f"  Existing non-blank pages: {len(existing_pages)}")
        for pg in existing_pages[:5]:
            print(f"    - {pg.url}")

        # Reuse a New Tab if there is one; otherwise create a fresh page.
        target = None
        for pg in ctx.pages:
            if pg.url in ("about:blank", "chrome://newtab/", ""):
                target = pg
                break
        if target is None:
            target = ctx.new_page()
            print("  Created a new page")
        else:
            print(f"  Reusing existing page: {target.url!r}")

        print(f"\nNavigating to {CREATOR_URL} ...")
        target.goto(CREATOR_URL, wait_until="domcontentloaded", timeout=30_000)
        time.sleep(3)  # let the SPA settle / redirects finish

        url = target.url
        title = target.title()
        print(f"  URL:   {url}")
        print(f"  Title: {title}")

        out = screenshot_path("01_creator_landing.png")
        target.screenshot(path=str(out), full_page=False)
        print(f"  Screenshot: {out}")

        # Heuristics
        url_lower = url.lower()
        title_lower = title.lower()
        if (
            "passport" in url_lower
            or "/login" in url_lower
            or "登录" in title
            or "login" in title_lower
        ):
            print("\n✗ NOT logged in — landed on a login page.")
            raise SystemExit(2)
        elif "/creator-micro" in url_lower or "creator.douyin.com" in url_lower:
            print("\n✓ On the creator center — likely logged in.")
        else:
            print("\n? Ambiguous — inspect the screenshot.")


if __name__ == "__main__":
    main()
