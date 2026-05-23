# /// script
# requires-python = ">=3.10"
# dependencies = ["playwright>=1.49", "python-dotenv>=1.0", "httpx>=0.27"]
# ///
"""Wait for Douyin's post-upload form to fully render, then dump selectors.

06_test_upload kicked off the upload but the page was still rendering when
the probe ran. This script connects to the existing tab, dismisses any
'我知道了' onboarding tooltip, polls until visible text-inputs appear, then
records the selectors for title / caption / tags / visibility / Publish.

Read-only: doesn't click Publish.

Run: uv run scripts/douyin/07_probe_form.py
"""

from __future__ import annotations

import json
import time

from playwright.sync_api import sync_playwright

from common import cdp_ws_url, screenshot_path


WAIT_SECS = 180


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
            raise SystemExit("no creator.douyin.com tab")
        print(f"Attached to: {target.url}")

        # Dismiss any "我知道了" onboarding tooltip first.
        dismissed = target.evaluate(
            """
            () => {
              for (const el of document.querySelectorAll('button, div, span, a')) {
                const t = (el.innerText || '').trim();
                if (t === '我知道了' || t === '知道了' || t === '关闭') {
                  const r = el.getBoundingClientRect();
                  if (r.width > 0 && r.height > 0) {
                    el.click();
                    return t;
                  }
                }
              }
              return null;
            }
            """
        )
        if dismissed:
            print(f"  dismissed tooltip button: {dismissed!r}")
        else:
            print("  no onboarding tooltip to dismiss")

        time.sleep(1)

        # Poll for the form. We declare success when at least one text-y
        # input becomes visible (title field is usually the first to render).
        print(f"\nPolling up to {WAIT_SECS}s for the form ...")
        deadline = time.time() + WAIT_SECS
        n_inputs = 0
        while time.time() < deadline:
            n_inputs = target.evaluate(
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
            if n_inputs > 0:
                elapsed = int(WAIT_SECS - (deadline - time.time()))
                print(f"  ✓ {n_inputs} text-input(s) visible after ~{elapsed}s")
                break
            time.sleep(3)
        if n_inputs == 0:
            print(f"  ✗ form never rendered within {WAIT_SECS}s")

        # Settle for re-renders.
        time.sleep(2)
        out = screenshot_path("07_form_ready.png")
        target.screenshot(path=str(out), full_page=False)
        print(f"\nScreenshot: {out}")

        # Full selector dump.
        probe = target.evaluate(
            """
            () => {
              const sumEl = (el) => {
                if (!el) return null;
                const r = el.getBoundingClientRect();
                const dataAttrs = {};
                for (const a of el.attributes || []) {
                  if (a.name.startsWith('data-')) dataAttrs[a.name] = a.value;
                }
                return {
                  tag: el.tagName.toLowerCase(),
                  type: el.type || null,
                  placeholder: el.placeholder || null,
                  aria_label: el.getAttribute('aria-label') || null,
                  contenteditable: el.getAttribute('contenteditable') || null,
                  cls: (el.className || '').toString().slice(0, 120),
                  text: (el.innerText || '').trim().slice(0, 100),
                  data: dataAttrs,
                  x: Math.round(r.x), y: Math.round(r.y),
                  w: Math.round(r.width), h: Math.round(r.height),
                };
              };
              const inputs = Array.from(
                document.querySelectorAll('input[type="text"], textarea, [contenteditable="true"]')
              ).map(sumEl).filter(e => e && e.w > 50 && e.h > 10);

              const wanted = ['发布', '存草稿', '保存草稿', '取消', '上一步', '定时发布', '下一步'];
              const btns = [];
              for (const el of document.querySelectorAll('button, [role="button"], div, span')) {
                const text = (el.innerText || '').trim();
                if (wanted.some(w => text === w)) {
                  const r = el.getBoundingClientRect();
                  if (r.width >= 40 && r.height >= 20 && r.width <= 300) {
                    btns.push(sumEl(el));
                  }
                }
              }

              const visBits = [];
              for (const el of document.querySelectorAll('label, span, div')) {
                const t = (el.innerText || '').trim();
                if (/^(公开|好友可见|仅自己|私密|定时|立即发布)$/.test(t)) {
                  visBits.push(sumEl(el));
                }
              }

              return {
                url: location.href,
                inputs: inputs.slice(0, 15),
                buttons: btns.slice(0, 15),
                visibility: visBits.slice(0, 15),
              };
            }
            """
        )

        print(f"\nURL: {probe['url']}\n")
        print(f"Text inputs ({len(probe['inputs'])}):")
        for it in probe["inputs"]:
            print(
                f"  {it['tag']:8} ce={it['contenteditable']!r:8} placeholder={it['placeholder']!r:30} "
                f"@ ({it['x']},{it['y']}) {it['w']}x{it['h']}"
            )
            if it["cls"]:
                print(f"           cls={it['cls']!r}")

        print(f"\nAction buttons ({len(probe['buttons'])}):")
        for b in probe["buttons"]:
            print(
                f"  {b['tag']:6} {b['text']!r:18} @ ({b['x']},{b['y']}) {b['w']}x{b['h']}  cls={b['cls']!r}"
            )

        print(f"\nVisibility labels ({len(probe['visibility'])}):")
        for v in probe["visibility"]:
            print(f"  {v['tag']:6} {v['text']!r:14} @ ({v['x']},{v['y']})")


if __name__ == "__main__":
    main()
