# /// script
# requires-python = ">=3.10"
# dependencies = ["python-dotenv>=1.0"]
# ///
"""Open the Twitter compose dialog and identify the textarea + Post button.

Read-only. Navigates to https://x.com/compose/post (the direct compose URL),
waits for the textarea to appear, takes a screenshot, and dumps the
data-testid values of the compose UI for downstream selectors.

Does NOT post anything. The compose textarea is empty after this script.

Run: uv run scripts/twitter/02_open_compose.py
"""

from __future__ import annotations

import json

from common import pwright, screenshot_to


COMPOSE_URL = "https://x.com/compose/post"

# Known Twitter compose data-testids (stable across web releases as of 2026)
TEXTAREA_TESTID = "tweetTextarea_0"
POST_BUTTON_TESTID = "tweetButton"           # the modal's submit
POST_INLINE_TESTID = "tweetButtonInline"     # the timeline-inline submit


def main() -> None:
    print(f"Navigating to {COMPOSE_URL} ...")
    pwright("goto", COMPOSE_URL)

    # Wait for the compose textarea to be present in the DOM
    print(f"Waiting for textarea [data-testid='{TEXTAREA_TESTID}'] ...")
    pwright("wait-for", f"[data-testid='{TEXTAREA_TESTID}']", timeout=20)

    out = screenshot_to("02_compose_open.png")
    print(f"  ✓ screenshot saved: {out}")

    # Probe compose-UI elements via JS so we know what selectors work
    print("\nProbing compose-UI elements:")
    probe_js = """
    (() => {
      const probe = (sel) => {
        const el = document.querySelector(sel);
        if (!el) return {found: false};
        const rect = el.getBoundingClientRect();
        return {
          found: true,
          tag: el.tagName,
          role: el.getAttribute('role'),
          ariaLabel: el.getAttribute('aria-label'),
          x: Math.round(rect.x), y: Math.round(rect.y),
          w: Math.round(rect.width), h: Math.round(rect.height),
        };
      };
      return JSON.stringify({
        textarea: probe('[data-testid="tweetTextarea_0"]'),
        post_button: probe('[data-testid="tweetButton"]'),
        post_inline: probe('[data-testid="tweetButtonInline"]'),
        char_counter: probe('[data-testid="characterCount"]'),
        media_button: probe('[data-testid="fileInput"]'),
        emoji_button: probe('[aria-label="Add emoji"]'),
        location: 'url=' + location.href,
      });
    })()
    """
    res = pwright("eval", probe_js.strip())
    # pwright wraps the result in quotes; the value is a JSON string we need to parse
    raw = res.stdout.strip()
    # Extract the first JSON-looking blob from output
    start = raw.find('"{')
    if start != -1:
        end = raw.rfind('}"') + 2
        json_string = raw[start + 1 : end - 1]  # strip surrounding quotes
        # Unescape JSON-in-JSON
        json_string = json_string.replace('\\"', '"').replace('\\\\', '\\')
        try:
            data = json.loads(json_string)
            for key, val in data.items():
                print(f"  {key:14} = {val}")
        except json.JSONDecodeError:
            print(f"  could not parse JSON; raw stdout follows:")
            print(raw[:800])
    else:
        print(raw[:800])


if __name__ == "__main__":
    main()
