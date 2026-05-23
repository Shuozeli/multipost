# /// script
# requires-python = ">=3.10"
# dependencies = ["httpx>=0.27", "python-dotenv>=1.0"]
# ///
"""Verify WeChat MP credentials work by fetching a stable access token.

Read-only. Calls cgi-bin/stable_token. Caches result to token_cache.json.

Run: uv run scripts/wechat-mp/01_check_token.py
"""

from __future__ import annotations

import time

from common import TOKEN_CACHE, get_stable_token, load_credentials


def main() -> None:
    creds = load_credentials()
    print(f"Account: {creds.account_name}")
    print(f"AppID:   {creds.appid}")
    print()

    token = get_stable_token(force_refresh=True)
    print(f"✓ access_token obtained ({len(token)} chars): {token[:12]}...{token[-6:]}")
    print(f"  cached at: {TOKEN_CACHE}")
    print(f"  fetched at: {time.strftime('%Y-%m-%dT%H:%M:%S%z')}")


if __name__ == "__main__":
    main()
