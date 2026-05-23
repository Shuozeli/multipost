# /// script
# requires-python = ">=3.10"
# dependencies = ["httpx>=0.27", "python-dotenv>=1.0"]
# ///
"""List videos on the authenticated channel.

Uses the channel's "uploads" playlist (every channel has one). Quota cost:
1 unit per page of up to 50 items.

Run: uv run scripts/youtube/07_list_my_uploads.py
"""

from __future__ import annotations

from common import pretty, yt_get_oauth


def main() -> None:
    # Find our uploads playlist ID
    ch = yt_get_oauth("/channels", {"part": "contentDetails", "mine": "true"})
    if "error" in ch:
        raise SystemExit(pretty(ch))
    items = ch.get("items", [])
    if not items:
        raise SystemExit("No channel found.")
    uploads_pl = items[0]["contentDetails"]["relatedPlaylists"]["uploads"]
    print(f"Uploads playlist: {uploads_pl}\n")

    # Page through it
    page_token: str | None = None
    total = 0
    while True:
        params = {
            "part": "snippet,contentDetails,status",
            "playlistId": uploads_pl,
            "maxResults": 50,
        }
        if page_token:
            params["pageToken"] = page_token
        page = yt_get_oauth("/playlistItems", params)
        if "error" in page:
            raise SystemExit(pretty(page))

        for it in page.get("items", []):
            total += 1
            s = it["snippet"]
            cd = it.get("contentDetails", {})
            st = it.get("status", {})
            print(f"  {total:3}. {s['title']}")
            print(f"        videoId:   {cd.get('videoId')}")
            print(f"        published: {cd.get('videoPublishedAt', s.get('publishedAt'))}")
            print(f"        privacy:   {st.get('privacyStatus', '?')}")
            print(f"        url:       https://youtu.be/{cd.get('videoId')}")
            print()

        page_token = page.get("nextPageToken")
        if not page_token:
            break

    if total == 0:
        print("(no uploads on this channel yet)")
    else:
        print(f"Total: {total} videos")


if __name__ == "__main__":
    main()
