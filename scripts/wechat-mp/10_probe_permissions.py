# /// script
# requires-python = ">=3.10"
# dependencies = ["httpx>=0.27", "python-dotenv>=1.0", "Pillow>=10.0"]
# ///
"""Probe which WeChat MP read APIs this account is authorized for.

Diagnostic: hit several read endpoints with a minimal valid payload
and report which succeed / which return 48001 (unauthorized).

Run: uv run scripts/wechat-mp/10_probe_permissions.py
"""

from __future__ import annotations

import httpx

from common import API_BASE, get_stable_token

# (method, path, params, json_body, description)
PROBES = [
    ("GET",  "/cgi-bin/account/getaccountbasicinfo", {}, None,
        "Account basic info"),
    ("POST", "/cgi-bin/draft/batchget",   {}, {"offset": 0, "count": 1, "no_content": 1},
        "List drafts (private articles)"),
    ("POST", "/cgi-bin/draft/count",      {}, None,
        "Count drafts"),
    ("POST", "/cgi-bin/freepublish/batchget", {}, {"offset": 0, "count": 1, "no_content": 1},
        "List PUBLISHED articles (freepublish system — modern manual posts go here)"),
    ("POST", "/cgi-bin/material/get_materialcount", {}, None,
        "Count items in legacy material library"),
    ("POST", "/cgi-bin/material/batchget_material", {}, {"type": "news", "offset": 0, "count": 1},
        "List 'news' in legacy material library (pre-freepublish era)"),
    ("POST", "/cgi-bin/material/batchget_material", {}, {"type": "image", "offset": 0, "count": 1},
        "List 'image' in legacy material library"),
    ("GET",  "/cgi-bin/get_api_domain_ip", {}, None,
        "List API server IPs (utility)"),
    ("GET",  "/cgi-bin/get_current_autoreply_info", {}, None,
        "Autoreply config (read)"),
    ("POST", "/cgi-bin/message/mass/speed/get", {}, None,
        "Mass-send speed (group-send API)"),
    ("POST", "/cgi-bin/tags/get", {}, None,
        "List user tags"),
    ("POST", "/cgi-bin/user/get", {}, None,
        "List followers (paginated)"),
]


def main() -> None:
    token = get_stable_token()
    print(f"{'STATUS':<10} {'errcode':<8} {'METHOD':<6} ENDPOINT")
    print("-" * 100)

    for method, path, params, body, desc in PROBES:
        params = {**params, "access_token": token}
        try:
            if method == "GET":
                resp = httpx.get(f"{API_BASE}{path}", params=params, timeout=10.0)
            else:
                resp = httpx.post(f"{API_BASE}{path}", params=params, json=body, timeout=10.0)
            data = resp.json()
            errcode = data.get("errcode", 0)
        except Exception as e:
            errcode = -1
            data = {"errmsg": str(e)}

        if errcode == 0:
            status = "✓ OK"
        elif errcode == 48001:
            status = "✗ unauth"
        elif errcode == 40164:
            status = "✗ ip-wl"
        else:
            status = "? err"

        print(f"{status:<10} {errcode:<8} {method:<6} {path}")
        print(f"           {' '*8} {' '*6} → {desc}")
        if errcode not in (0, 48001):
            print(f"           {' '*8} {' '*6}   errmsg: {data.get('errmsg', '?')}")
        print()


if __name__ == "__main__":
    main()
