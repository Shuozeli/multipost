# /// script
# requires-python = ">=3.10"
# dependencies = ["httpx>=0.27", "python-dotenv>=1.0"]
# ///
"""Run the YouTube OAuth 2.0 authorization-code flow.

Spawns a one-shot HTTP server on YOUTUBE_OAUTH_PORT, prints an auth URL,
waits for Google to redirect back with `?code=...`, exchanges the code
for access + refresh tokens, and saves them to tokens.json.

If you're running this on a remote machine over SSH, FIRST set up
port-forwarding on your local laptop:

    ssh -L 8765:localhost:8765 <this-host>

That way when your browser hits http://localhost:8765/... after Google
redirects, the request reaches the script's listener on this server.

Run: uv run scripts/youtube/05_oauth_login.py
"""

from __future__ import annotations

import http.server
import secrets
import socketserver
import threading
import time
import urllib.parse

import httpx

from common import (
    TOKEN_ENDPOINT,
    oauth_client_id,
    oauth_client_secret,
    oauth_port,
    save_tokens,
)


SCOPES = [
    "https://www.googleapis.com/auth/youtube.readonly",   # list my channel, my videos
    "https://www.googleapis.com/auth/youtube.upload",      # upload videos
    "https://www.googleapis.com/auth/youtube",             # full read+write (covers edit/delete)
]

AUTH_URL = "https://accounts.google.com/o/oauth2/v2/auth"


def main() -> None:
    port = oauth_port()
    state = secrets.token_urlsafe(32)
    redirect_uri = f"http://localhost:{port}"

    params = {
        "client_id": oauth_client_id(),
        "redirect_uri": redirect_uri,
        "response_type": "code",
        "scope": " ".join(SCOPES),
        "state": state,
        "access_type": "offline",   # request refresh_token
        "prompt": "consent",        # force the consent screen so we get refresh_token
                                    # even if user already authorized previously
    }
    auth_url = f"{AUTH_URL}?{urllib.parse.urlencode(params)}"

    # Write URL to a side file so it's recoverable even with buffered stdout
    from pathlib import Path
    Path(__file__).resolve().parent.joinpath("auth_url.txt").write_text(auth_url + "\n")

    print("=" * 70, flush=True)
    print("Open this URL in your browser (logged into the YouTube account you", flush=True)
    print("want to authorize):\n", flush=True)
    print(f"  {auth_url}\n", flush=True)
    print(f"If on a remote machine over SSH, first run on your laptop:", flush=True)
    print(f"  ssh -L {port}:localhost:{port} <this-host>", flush=True)
    print("=" * 70, flush=True)
    print(f"\nWaiting for callback on port {port} ...", flush=True)
    print(f"(URL also saved to scripts/youtube/auth_url.txt)", flush=True)

    received: dict[str, str | None] = {}

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):  # noqa: N802
            parsed = urllib.parse.urlparse(self.path)
            qs = urllib.parse.parse_qs(parsed.query)
            received["code"] = qs.get("code", [None])[0]
            received["state"] = qs.get("state", [None])[0]
            received["error"] = qs.get("error", [None])[0]
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.end_headers()
            if received.get("error"):
                body = f"<h1>Auth failed: {received['error']}</h1>"
            else:
                body = "<h1>Auth received. You can close this tab.</h1>"
            self.wfile.write(body.encode("utf-8"))

        def log_message(self, *args):  # silence access log
            pass

    class ReuseAddrServer(socketserver.TCPServer):
        allow_reuse_address = True

    with ReuseAddrServer(("0.0.0.0", port), Handler) as server:
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        deadline = time.time() + 300  # 5 minutes
        while not received and time.time() < deadline:
            time.sleep(0.5)
        server.shutdown()

    if not received:
        raise SystemExit("Timed out waiting for callback (5 min). Aborting.")
    if received.get("error"):
        raise SystemExit(f"OAuth error: {received['error']}")
    if received.get("state") != state:
        raise SystemExit("State mismatch — possible CSRF or stale callback.")
    code = received["code"]
    if not code:
        raise SystemExit("No code in callback — Google didn't return an authorization code.")

    print(f"  ✓ Got authorization code (truncated): {code[:24]}...")
    print("  Exchanging for access + refresh tokens...")

    resp = httpx.post(TOKEN_ENDPOINT, data={
        "code": code,
        "client_id": oauth_client_id(),
        "client_secret": oauth_client_secret(),
        "redirect_uri": redirect_uri,
        "grant_type": "authorization_code",
    }, timeout=15.0)
    if resp.status_code != 200:
        print(resp.text)
        raise SystemExit(f"Token exchange failed: {resp.status_code}")

    tokens = resp.json()
    save_tokens(tokens)

    print(f"\n✓ Tokens saved.")
    print(f"  access_token (truncated): {tokens['access_token'][:32]}...")
    print(f"  refresh_token present:    {'yes' if tokens.get('refresh_token') else 'NO (re-run with prompt=consent)'}")
    print(f"  expires_in:               {tokens.get('expires_in')} seconds")
    print(f"\nNext: uv run scripts/youtube/06_my_channel.py")


if __name__ == "__main__":
    main()
