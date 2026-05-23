# /// script
# requires-python = ">=3.10"
# dependencies = ["httpx>=0.27", "python-dotenv>=1.0"]
# ///
"""Fetch basic info about the connected WeChat MP account.

Read-only. Calls cgi-bin/account/getaccountbasicinfo to verify we are
authenticated against the expected account (nickname, principal, etc.).

Run: uv run scripts/wechat-mp/02_account_info.py
"""

from __future__ import annotations

import httpx

from common import API_BASE, get_stable_token, pretty


def main() -> None:
    token = get_stable_token()
    resp = httpx.get(
        f"{API_BASE}/cgi-bin/account/getaccountbasicinfo",
        params={"access_token": token},
        timeout=10.0,
    )
    resp.raise_for_status()
    data = resp.json()
    print(pretty(data))


if __name__ == "__main__":
    main()
