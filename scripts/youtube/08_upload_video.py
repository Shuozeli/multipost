# /// script
# requires-python = ">=3.10"
# dependencies = ["httpx>=0.27", "python-dotenv>=1.0"]
# ///
"""Upload a video file to the authenticated YouTube channel.

DEFAULTS TO PRIVATE. You'd have to pass --privacy public to make it
visible to viewers, AND YouTube's audit system may still demote it
in the first few minutes (copyright, content checks).

Quota: 1,600 units per upload. Default daily quota is 10,000, so
~6 uploads/day before hitting the cap.

Usage:
  uv run scripts/youtube/08_upload_video.py --file path/to/video.mp4
  uv run scripts/youtube/08_upload_video.py --file path/to/video.mp4 \\
      --title "My title" --description "..." --tags "ai,test"
  uv run scripts/youtube/08_upload_video.py --file ... --privacy unlisted
  uv run scripts/youtube/08_upload_video.py --file ... --privacy public
"""

from __future__ import annotations

import argparse
import json
import mimetypes
from pathlib import Path

import httpx

from common import get_access_token


# Resumable upload endpoint (different from the read API base URL)
UPLOAD_URL = "https://www.googleapis.com/upload/youtube/v3/videos"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--file", required=True, help="Path to the video file")
    parser.add_argument("--title", default="multipost upload test",
                        help="Video title (max 100 chars)")
    parser.add_argument("--description", default=(
        "Automated upload test from multipost prototype. "
        "Private by default; delete if seen."
    ))
    parser.add_argument("--tags", default="multipost,test",
                        help="Comma-separated tags")
    parser.add_argument("--category", default="22",
                        help="YouTube category ID (22=People & Blogs, "
                             "27=Education, 28=Sci/Tech, 25=News/Politics)")
    parser.add_argument("--privacy", default="private",
                        choices=["private", "unlisted", "public"])
    parser.add_argument("--made-for-kids", action="store_true",
                        help="Mark video as made for kids (default: NOT for kids)")
    args = parser.parse_args()

    path = Path(args.file).expanduser().resolve()
    if not path.is_file():
        raise SystemExit(f"File not found: {path}")
    size = path.stat().st_size
    mime, _ = mimetypes.guess_type(str(path))
    mime = mime or "video/*"

    print(f"File:        {path}")
    print(f"Size:        {size:,} bytes ({size / 1_048_576:.2f} MB)")
    print(f"MIME:        {mime}")
    print(f"Title:       {args.title!r}")
    print(f"Privacy:     {args.privacy}")
    print(f"Category:    {args.category}")
    print(f"For kids:    {args.made_for_kids}")

    if len(args.title) > 100:
        raise SystemExit("Title exceeds 100 chars (YouTube hard limit).")

    metadata = {
        "snippet": {
            "title": args.title,
            "description": args.description,
            "tags": [t.strip() for t in args.tags.split(",") if t.strip()],
            "categoryId": args.category,
        },
        "status": {
            "privacyStatus": args.privacy,
            "selfDeclaredMadeForKids": args.made_for_kids,
            "embeddable": True,
        },
    }

    print("\nPhase 1: initialize resumable upload")
    init_resp = httpx.post(
        UPLOAD_URL,
        params={"uploadType": "resumable", "part": "snippet,status"},
        headers={
            "Authorization": f"Bearer {get_access_token()}",
            "Content-Type": "application/json; charset=UTF-8",
            "X-Upload-Content-Type": mime,
            "X-Upload-Content-Length": str(size),
        },
        content=json.dumps(metadata).encode("utf-8"),
        timeout=30.0,
    )
    if init_resp.status_code not in (200, 201):
        print(init_resp.text)
        raise SystemExit(f"Init failed: HTTP {init_resp.status_code}")
    upload_session_url = init_resp.headers.get("Location")
    if not upload_session_url:
        raise SystemExit("Init response missing Location header — can't continue")
    print(f"  ✓ session URL: {upload_session_url[:100]}...")

    print(f"\nPhase 2: PUT video bytes ({size:,} bytes)")
    with open(path, "rb") as f:
        body = f.read()
    upload_resp = httpx.put(
        upload_session_url,
        headers={"Content-Type": mime, "Content-Length": str(size)},
        content=body,
        timeout=600.0,  # generous: 10 min for large uploads
    )
    if upload_resp.status_code not in (200, 201):
        print(upload_resp.text)
        raise SystemExit(f"Upload PUT failed: HTTP {upload_resp.status_code}")

    result = upload_resp.json()
    vid_id = result.get("id")
    print(f"\n✓ Upload complete")
    print(f"  video ID:    {vid_id}")
    print(f"  url:         https://youtu.be/{vid_id}")
    print(f"  studio:      https://studio.youtube.com/video/{vid_id}/edit")
    print(f"  privacy:     {result.get('status', {}).get('privacyStatus')}")
    print(f"  upload status: {result.get('status', {}).get('uploadStatus')}")
    print(f"\nNote: 'upload_status: uploaded' means bytes arrived. YouTube still")
    print(f"runs format conversion + audit. Check the studio URL above to see")
    print(f"processing progress and any audit flags.")


if __name__ == "__main__":
    main()
