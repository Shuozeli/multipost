# /// script
# requires-python = ">=3.10"
# dependencies = ["httpx>=0.27", "python-dotenv>=1.0"]
# ///
"""Fallback for 05_oauth_login.py when the local-server callback isn't reachable.

Use this when you ran 05_oauth_login.py but couldn't SSH-forward port 8765
(or the redirect just failed in the browser). Copy the URL from your browser
after Google redirected you (it will look like
`http://localhost:8765/?code=...&state=...`) and paste it here.

Run: uv run scripts/youtube/05b_paste_code.py "<paste the URL or just the code>"
"""

from __future__ import annotations

import sys
import urllib.parse

import httpx

from common import (
    TOKEN_ENDPOINT,
    oauth_client_id,
    oauth_client_secret,
    oauth_port,
    save_tokens,
)


def extract_code(arg: str) -> str:
    if "?" in arg or "&" in arg:
        # treat as URL — parse the `code` param
        parsed = urllib.parse.urlparse(arg)
        qs = urllib.parse.parse_qs(parsed.query)
        code = qs.get("code", [None])[0]
        if not code:
            raise SystemExit(f"No 'code' parameter in URL: {arg}")
        return code
    return arg.strip()


def main() -> None:
    if len(sys.argv) < 2:
        raise SystemExit("Usage: 05b_paste_code.py '<paste URL or code>'")
    code = extract_code(sys.argv[1])
    print(f"Exchanging code ({code[:24]}...) for tokens...")

    resp = httpx.post(TOKEN_ENDPOINT, data={
        "code": code,
        "client_id": oauth_client_id(),
        "client_secret": oauth_client_secret(),
        "redirect_uri": f"http://localhost:{oauth_port()}",
        "grant_type": "authorization_code",
    }, timeout=15.0)

    if resp.status_code != 200:
        print(f"Token exchange failed: HTTP {resp.status_code}")
        print(resp.text)
        raise SystemExit(1)

    tokens = resp.json()
    save_tokens(tokens)
    print(f"✓ Tokens saved")
    print(f"  access_token (truncated): {tokens['access_token'][:32]}...")
    print(f"  refresh_token present:    {'yes' if tokens.get('refresh_token') else 'NO'}")
    print(f"  expires_in:               {tokens.get('expires_in')} seconds")
    print(f"  granted scope:            {tokens.get('scope')}")


if __name__ == "__main__":
    main()
