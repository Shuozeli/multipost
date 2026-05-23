# /// script
# requires-python = ">=3.10"
# dependencies = ["playwright>=1.49", "python-dotenv>=1.0", "httpx>=0.27"]
# ///
"""Extract the inline base64 QR code from the Douyin login panel.

Saves it as `screenshots/03_qr.png` so you can scan it in your image
viewer with the Douyin app on your phone.

Run: uv run scripts/douyin/03_extract_qr.py
"""

from __future__ import annotations

import base64

from playwright.sync_api import sync_playwright

from common import cdp_ws_url, screenshot_path


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
            raise SystemExit("no creator.douyin.com tab found — run 02_find_login.py first")

        # The QR code is an <img> inside the .scan_qrcode_login* container,
        # rendered with src="data:image/png;base64,...".
        data_url = target.evaluate(
            """
            () => {
              // Look inside the scan-code panel first
              const scopes = [
                document.querySelector('[class*="scan_qrcode_login"]'),
                document.querySelector('[class*="login"]'),
                document,
              ];
              for (const s of scopes) {
                if (!s) continue;
                for (const img of s.querySelectorAll('img')) {
                  const src = img.getAttribute('src') || '';
                  const r = img.getBoundingClientRect();
                  if (src.startsWith('data:image/') && r.width >= 100 && Math.abs(r.width - r.height) < 30) {
                    return src;
                  }
                }
              }
              return null;
            }
            """
        )
        if not data_url:
            raise SystemExit("no QR-shaped data: URL image found on the page")
        prefix, _, b64 = data_url.partition(",")
        if not b64:
            raise SystemExit(f"unexpected data URL shape: {prefix!r}")
        png_bytes = base64.b64decode(b64)
        out = screenshot_path("03_qr.png")
        out.write_bytes(png_bytes)
        print(f"  ✓ QR saved: {out}  ({len(png_bytes):,} bytes)")
        print(f"  Open this file in your image viewer and scan with the Douyin app.")


if __name__ == "__main__":
    main()
