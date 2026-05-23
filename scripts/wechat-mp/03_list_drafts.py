# /// script
# requires-python = ">=3.10"
# dependencies = ["httpx>=0.27", "python-dotenv>=1.0"]
# ///
"""List existing drafts on the account.

Read-only. Calls cgi-bin/draft/batchget.

Run: uv run scripts/wechat-mp/03_list_drafts.py
"""

from __future__ import annotations

import httpx

from common import API_BASE, get_stable_token, pretty


def main() -> None:
    token = get_stable_token()
    resp = httpx.post(
        f"{API_BASE}/cgi-bin/draft/batchget",
        params={"access_token": token},
        json={"offset": 0, "count": 10, "no_content": 1},
        timeout=10.0,
    )
    resp.raise_for_status()
    data = resp.json()
    print(pretty(data))


if __name__ == "__main__":
    main()
