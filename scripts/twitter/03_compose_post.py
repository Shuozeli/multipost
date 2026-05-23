# /// script
# requires-python = ">=3.10"
# dependencies = ["python-dotenv>=1.0"]
# ///
"""Compose (and optionally post) a tweet.

DEFAULTS TO DRY RUN. The text is typed into the compose box, a screenshot
is taken, then the dialog is dismissed without posting. To actually publish
the tweet, pass `--post`.

Usage:
  uv run scripts/twitter/03_compose_post.py
    → opens compose, types default text, screenshots, dismisses

  uv run scripts/twitter/03_compose_post.py --text "Hello from multipost"
    → same with custom text

  uv run scripts/twitter/03_compose_post.py --text "..." --post
    → actually clicks Post. Irreversible.
"""

from __future__ import annotations

import argparse
import time

from common import pwright, screenshot_to


# Use /home's inline composer rather than /compose/post. The /compose/post URL
# opens a MODAL on top of the timeline, leaving TWO `tweetTextarea_0` elements
# in the DOM — querySelector picks the first (inline) and the modal stays empty.
# The inline composer is unambiguous and has a `tweetButtonInline` post button.
COMPOSE_URL = "https://x.com/home"
TEXTAREA_SEL = '[data-testid="tweetTextarea_0"]'
POST_BUTTON_SEL = '[data-testid="tweetButtonInline"]'

DEFAULT_TEXT = (
    f"[multipost dry-run] {time.strftime('%Y-%m-%dT%H:%M:%S')} — "
    "automation prototype, please ignore."
)


def insert_text(text: str) -> None:
    """Focus the textarea and insert text in ONE eval call.

    Why one call: every pwright invocation opens a fresh CDP session, which
    can blur focus between commands. Doing focus + insert atomically inside
    a single page-side function avoids that race.

    Why execCommand('insertText'): the textarea is a Draft.js-managed
    contenteditable. Direct .innerText/.innerHTML writes don't trigger
    React/Draft state updates, so the Post button stays disabled even
    though the text visibly appears. execCommand('insertText') goes through
    the proper InputEvent path that Draft.js listens for.
    """
    # JSON-encode so embedded quotes/newlines survive shell + JS parsing
    import json as _json
    payload = _json.dumps(text)
    # KNOWN QUIRK: Twitter's composer is a Lexical/Draft-style editor.
    # `execCommand('insertText', ...)` updates BOTH the DOM and the editor's
    # React state correctly — Post button enables.
    # Do NOT precede with selectAll+delete: that desyncs the React model
    # (DOM gets cleared, React state stays — then insertText only updates
    # DOM, leaving the editor's model "empty" from React's perspective).
    # If a leftover draft is in the composer, manually clear via the UI
    # (X button at top of compose dropdown) before rerunning.
    js = (
        "(() => {"
        f" const el = document.querySelector('{TEXTAREA_SEL}');"
        " if (!el) return 'NO_TEXTAREA';"
        " el.focus();"
        f" document.execCommand('insertText', false, {payload});"
        " return el.innerText;"
        "})()"
    )
    res = pwright("eval", js)
    print(f"   textarea innerText after insert: {res.stdout.strip()[:200]}")


def post_button_state() -> dict:
    """Check whether the inline composer's Post button is enabled."""
    js = f"""
    (() => {{
      const btn = document.querySelector('{POST_BUTTON_SEL}');
      if (!btn) return JSON.stringify({{found: false}});
      return JSON.stringify({{
        found: true,
        disabled: btn.disabled || btn.getAttribute('aria-disabled') === 'true',
        text: btn.innerText,
      }});
    }})()
    """
    res = pwright("eval", js.strip())
    # parse out the JSON string from the pwright output
    raw = res.stdout
    start = raw.find('"{')
    if start == -1:
        return {"found": False, "raw": raw}
    end = raw.rfind('}"') + 2
    import json
    return json.loads(raw[start + 1 : end - 1].replace('\\"', '"'))


def dismiss_compose() -> None:
    """Press Escape to close the compose dialog without posting."""
    pwright("press", "Escape")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--text", default=DEFAULT_TEXT,
                        help="Tweet body. Default: timestamped dry-run notice.")
    parser.add_argument("--post", action="store_true",
                        help="Actually click Post. Without this flag, we dry-run only.")
    args = parser.parse_args()

    text = args.text
    print(f"Mode:  {'POST' if args.post else 'dry-run'}")
    print(f"Text:  {text!r}")
    print(f"Length: {len(text)} chars (Twitter limit: 280)")
    if len(text) > 280:
        raise SystemExit("Text exceeds 280 chars. Twitter would reject this.")

    print(f"\n1. Open {COMPOSE_URL}")
    pwright("goto", COMPOSE_URL)

    print("2. Wait for textarea")
    pwright("wait-for", TEXTAREA_SEL, timeout=20)
    time.sleep(1.5)  # let React fully mount + register listeners

    print("3. Focus + insertText in one eval call")
    insert_text(text)
    time.sleep(1.0)  # let React process the input event and re-render

    state = post_button_state()
    print(f"4. Post button state: {state}")

    out = screenshot_to(f"03_composed_{'live' if args.post else 'dry'}.png")
    print(f"5. Screenshot: {out}")

    if not args.post:
        print("\n6. DRY RUN — dismissing without posting.")
        dismiss_compose()
        time.sleep(0.5)
        out2 = screenshot_to("03_dismissed.png")
        print(f"   Post-dismiss screenshot: {out2}")
        return

    # --- LIVE PATH ---
    if not state.get("found") or state.get("disabled"):
        raise SystemExit(
            f"Post button not ready (state={state}). Aborting before clicking."
        )

    print("\n6. LIVE — clicking Post button NOW")
    pwright("eval", f"document.querySelector('{POST_BUTTON_SEL}').click()")
    time.sleep(3.0)

    # capture state after submission
    url_res = pwright("eval", "location.href")
    print(f"7. Post-submit URL: {url_res.stdout.strip()}")
    out3 = screenshot_to("03_after_post.png")
    print(f"   After-post screenshot: {out3}")
    print("\n✓ Tweet submitted (verify on screenshot and timeline)")


if __name__ == "__main__":
    main()
