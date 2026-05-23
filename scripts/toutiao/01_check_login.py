# /// script
# requires-python = ">=3.10"
# dependencies = ["playwright>=1.49", "python-dotenv>=1.0", "httpx>=0.27"]
# ///
"""Open Toutiao's creator portal and check login state.

Toutiao's creator portal is `https://mp.toutiao.com/`. Logged-in users
land on a dashboard at `/profile_v4/index` or similar; logged-out users
get a QR-code splash.

Run: uv run scripts/toutiao/01_check_login.py
"""

from __future__ import annotations

import time

from playwright.sync_api import sync_playwright

from common import cdp_ws_url, screenshot_path


CREATOR_URL = "https://mp.toutiao.com/"


def main() -> None:
    ws = cdp_ws_url()
    print(f"Connecting to CDP over WS: {ws}")

    with sync_playwright() as p:
        browser = p.chromium.connect_over_cdp(ws)
        print(f"  ✓ Chrome/{browser.version}")
        ctx = browser.contexts[0]
        non_blank = [pg for pg in ctx.pages if pg.url and pg.url != "about:blank"]
        print(f"  Existing pages ({len(non_blank)}):")
        for pg in non_blank[:5]:
            print(f"    - {pg.url}")

        # Reuse an existing toutiao tab if there is one, else create a fresh page.
        target = None
        for pg in ctx.pages:
            if "toutiao.com" in pg.url:
                target = pg
                break
        if target is None:
            target = ctx.new_page()
            print("  Created a new page")
        else:
            print(f"  Reusing existing toutiao tab: {target.url!r}")

        print(f"\nNavigating to {CREATOR_URL} ...")
        target.goto(CREATOR_URL, wait_until="domcontentloaded", timeout=30_000)
        time.sleep(3)

        url = target.url
        title = target.title()
        print(f"\n  URL:   {url}")
        print(f"  Title: {title}")

        out = screenshot_path("01_landing.png")
        target.screenshot(path=str(out), full_page=False)
        print(f"  Screenshot: {out}")

        url_lower = url.lower()
        title_lower = title.lower()
        if (
            "login" in url_lower
            or "passport" in url_lower
            or "登录" in title
            or "login" in title_lower
            or "sso." in url_lower
        ):
            print("\n✗ NOT logged in — looks like a login page.")
        elif "mp.toutiao.com" in url_lower:
            print("\n✓ On mp.toutiao.com — check screenshot for dashboard vs splash.")
        else:
            print("\n? Ambiguous — inspect the screenshot.")


if __name__ == "__main__":
    main()
