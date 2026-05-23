# /// script
# requires-python = ">=3.10"
# dependencies = ["python-dotenv>=1.0"]
# ///
"""Verify we're logged into Twitter via the shared Chrome's user profile.

Read-only. Drives the existing tab (pwright session state in .pwright/),
gets the current URL, takes a screenshot, asserts URL contains /home.

Run: uv run scripts/twitter/01_check_login.py
"""

from __future__ import annotations

from common import pwright, screenshot_to


def get_url() -> str:
    """Eval `location.href` to determine current page."""
    res = pwright("eval", "location.href")
    # pwright eval prints the result as JSON-ish; look for the URL line
    for line in res.stdout.splitlines():
        line = line.strip()
        if line.startswith("\"") and "http" in line:
            return line.strip('"')
        if line.startswith("http"):
            return line.split()[0]
    raise RuntimeError(f"couldn't parse URL from:\n{res.stdout}")


def get_title() -> str:
    res = pwright("eval", "document.title")
    for line in res.stdout.splitlines():
        line = line.strip()
        if line.startswith('"'):
            return line.strip('"')
    return ""


def main() -> None:
    print("Pinging pwright health...")
    res = pwright("health")
    print("  " + "\n  ".join(l for l in res.stdout.splitlines() if l.strip()))

    print("\nCurrent state:")
    url = get_url()
    title = get_title()
    print(f"  URL:   {url}")
    print(f"  Title: {title}")

    out = screenshot_to("01_login_check.png")
    print(f"  Screenshot: {out}")

    if "/home" in url or "logged" in title.lower() or "Home" in title:
        print("\n✓ Looks logged in.")
    elif "x.com/i/flow/login" in url or "Sign in" in title or "Log in" in title:
        print("\n✗ NOT logged in — landed on a login page.")
        raise SystemExit(1)
    else:
        print(f"\n? Ambiguous — neither /home nor /login. Inspect screenshot.")


if __name__ == "__main__":
    main()
