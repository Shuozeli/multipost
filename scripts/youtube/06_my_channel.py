# /// script
# requires-python = ">=3.10"
# dependencies = ["httpx>=0.27", "python-dotenv>=1.0"]
# ///
"""Verify OAuth by fetching the authenticated user's own channel.

Uses /channels?mine=true which requires the youtube.readonly scope.
Confirms upload eligibility (channel exists, region, etc.).

Run: uv run scripts/youtube/06_my_channel.py
"""

from __future__ import annotations

from common import pretty, yt_get_oauth


def main() -> None:
    parts = "snippet,statistics,contentDetails,status,brandingSettings"
    data = yt_get_oauth("/channels", {"part": parts, "mine": "true"})

    if "error" in data:
        raise SystemExit(pretty(data))
    items = data.get("items", [])
    if not items:
        raise SystemExit("No channel attached to this Google account. "
                         "Make sure the account has a YouTube channel created.")

    ch = items[0]
    snip = ch["snippet"]
    stats = ch.get("statistics", {})
    status = ch.get("status", {})
    uploads_pl = ch.get("contentDetails", {}).get("relatedPlaylists", {}).get("uploads")
    print(f"  Channel:       {snip['title']}")
    print(f"  Channel ID:    {ch['id']}")
    print(f"  Handle:        {snip.get('customUrl', '?')}")
    print(f"  Country:       {snip.get('country', '?')}")
    print(f"  Created:       {snip.get('publishedAt')}")
    print(f"  Subscribers:   {int(stats.get('subscriberCount', 0)):,}")
    print(f"  Videos:        {int(stats.get('videoCount', 0)):,}")
    print(f"  Total views:   {int(stats.get('viewCount', 0)):,}")
    print(f"  Privacy:       {status.get('privacyStatus', '?')}")
    print(f"  Long uploads:  {status.get('longUploadsStatus', '?')}  "
          "(allowed = phone-verified, can upload >15min videos)")
    print(f"  Made for kids: {status.get('madeForKids')}")
    print(f"  Uploads playlist: {uploads_pl}")
    print(f"\n  Description:")
    desc = (snip.get("description") or "").strip()
    print("    " + (desc[:400] or "(none)"))
    print("\n✓ OAuth working. Upload eligibility depends on longUploadsStatus above.")


if __name__ == "__main__":
    main()
